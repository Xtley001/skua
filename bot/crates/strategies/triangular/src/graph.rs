// crates/strategies/triangular/src/graph.rs
//
// Triangular arb graph: tokens are nodes, AMM pools are directed edges.
// Edge weight = -ln(effective_exchange_rate) — negative-weight cycle = profitable arb.
//
// For ≤ 15 tokens: DFS with depth limit 4.
// For > 15 tokens: Bellman-Ford (detects negative-weight cycles in O(VE)).
//
// Every candidate route is then sized via GSS (128 iterations).

use alloy::primitives::Address;
use skua_core::{
    gas::S3_TRI_ARB_3HOP_GAS_ESTIMATE,
    state::PoolState,
    types::{FlashProvider, SizingResult},
    BotState, SkuaConfig,
};
use skua_simulation::{net_profit_three_leg, optimal_borrow_size, AmmPool};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;

/// A single directed edge in the arb graph.
#[derive(Debug, Clone)]
pub struct PoolEdge {
    pub pool:         Address,
    pub token_out:    Address,
    pub fee_bps:      f64,
    pub reserve_in:   f64,
    pub reserve_out:  f64,
    pub amp:          f64,    // >0 for stable pools
    pub is_stable:    bool,
}

impl PoolEdge {
    /// Compute the effective exchange rate for one unit of token_in → token_out.
    /// Used for graph weight: weight = -ln(rate).
    pub fn effective_rate_per_unit(&self) -> f64 {
        let pool = AmmPool {
            reserve_in:  self.reserve_in,
            reserve_out: self.reserve_out,
            fee_bps:     self.fee_bps,
            amp:         self.amp,
            n_tokens:    2,
        };
        let unit = self.reserve_in * 0.0001; // 0.01% of reserve as unit probe
        if unit <= 0.0 { return 0.0; }
        let out = if self.is_stable {
            pool.get_output_stable(unit)
        } else {
            pool.get_output(unit)
        };
        if unit <= 0.0 { 0.0 } else { out / unit }
    }
}

/// A complete arb route (cycle back to the start token).
#[derive(Debug, Clone)]
pub struct ArbRoute {
    pub start_token:          Address,
    pub start_token_decimals: u8,
    pub hops: Vec<RouteHop>,
}

#[derive(Debug, Clone)]
pub struct RouteHop {
    pub pool:      Address,
    pub token_in:  Address,
    pub token_out: Address,
}

/// The arb opportunity graph.
/// Rebuilt on every pool state update (cheap — just a HashMap).
pub struct ArbGraph {
    /// token_in → Vec of outbound edges
    edges: HashMap<Address, Vec<PoolEdge>>,
}

impl ArbGraph {
    /// Build the graph from current BotState pool states.
    pub fn build(state: &Arc<BotState>) -> Self {
        let pool_states = state.pool_states.read();
        let mut edges: HashMap<Address, Vec<PoolEdge>> = HashMap::new();

        for (&pool_addr, ps) in pool_states.iter() {
            let rate_a_to_b = {
                let p = AmmPool {
                    reserve_in: ps.reserve_a as f64,
                    reserve_out: ps.reserve_b as f64,
                    fee_bps: ps.fee_bps as f64,
                    amp: ps.amp,
                    n_tokens: 2,
                };
                let unit = ps.reserve_a as f64 * 0.0001;
                if unit > 0.0 { p.get_output(unit) / unit } else { 0.0 }
            };

            let rate_b_to_a = {
                let p = AmmPool {
                    reserve_in: ps.reserve_b as f64,
                    reserve_out: ps.reserve_a as f64,
                    fee_bps: ps.fee_bps as f64,
                    amp: ps.amp,
                    n_tokens: 2,
                };
                let unit = ps.reserve_b as f64 * 0.0001;
                if unit > 0.0 { p.get_output(unit) / unit } else { 0.0 }
            };

            let is_stable = ps.amp > 0.0;

            // A → B
            edges.entry(ps.token_a).or_default().push(PoolEdge {
                pool:        pool_addr,
                token_out:   ps.token_b,
                fee_bps:     ps.fee_bps as f64,
                reserve_in:  ps.reserve_a as f64,
                reserve_out: ps.reserve_b as f64,
                amp:         ps.amp,
                is_stable,
            });

            // B → A
            edges.entry(ps.token_b).or_default().push(PoolEdge {
                pool:        pool_addr,
                token_out:   ps.token_a,
                fee_bps:     ps.fee_bps as f64,
                reserve_in:  ps.reserve_b as f64,
                reserve_out: ps.reserve_a as f64,
                amp:         ps.amp,
                is_stable,
            });
        }

        Self { edges }
    }

    /// Find all profitable 2–4 hop cycles.
    /// Dispatches to DFS (≤15 tokens) or Bellman-Ford (>15 tokens).
    pub fn find_cycles(&self) -> Vec<ArbRoute> {
        let token_count = self.edges.len();
        if token_count <= 15 {
            self.dfs_cycles(3) // 3-hop for S3
        } else {
            self.bellman_ford_cycles()
        }
    }

    /// DFS cycle detection with max_depth hops.
    fn dfs_cycles(&self, max_depth: usize) -> Vec<ArbRoute> {
        let mut routes = Vec::new();
        let tokens: Vec<Address> = self.edges.keys().copied().collect();

        for &start in &tokens {
            let mut path: Vec<RouteHop> = Vec::with_capacity(max_depth);
            let mut visited: Vec<Address> = vec![start];
            self.dfs_recurse(
                start,  // origin
                start,  // current
                &mut path,
                &mut visited,
                max_depth,
                &mut routes,
            );
        }

        routes
    }

    fn dfs_recurse(
        &self,
        origin:    Address,
        current:   Address,
        path:      &mut Vec<RouteHop>,
        visited:   &mut Vec<Address>,
        remaining: usize,
        out:       &mut Vec<ArbRoute>,
    ) {
        if remaining == 0 { return; }

        let outbound = match self.edges.get(&current) {
            Some(e) => e,
            None => return,
        };

        for edge in outbound {
            let next = edge.token_out;

            // If next == origin and we have at least 2 hops — found a cycle
            if next == origin && path.len() >= 2 {
                let mut hops = path.clone();
                hops.push(RouteHop {
                    pool:      edge.pool,
                    token_in:  current,
                    token_out: next,
                });
                // Quick profitability pre-filter: product of rates > 1
                if self.rates_product(&hops) > 1.0 {
                    out.push(ArbRoute {
                        start_token:          origin,
                        start_token_decimals: 6, // TODO: look up from token registry
                        hops,
                    });
                }
                continue;
            }

            if !visited.contains(&next) && remaining > 1 {
                visited.push(next);
                path.push(RouteHop {
                    pool:      edge.pool,
                    token_in:  current,
                    token_out: next,
                });
                self.dfs_recurse(origin, next, path, visited, remaining - 1, out);
                path.pop();
                visited.pop();
            }
        }
    }

    /// Bellman-Ford negative-weight cycle detection for larger token sets.
    /// Uses log-transformed edge weights: w(e) = -ln(rate(e)).
    /// Negative-weight cycle ↔ product of rates > 1 ↔ profitable arb.
    fn bellman_ford_cycles(&self) -> Vec<ArbRoute> {
        let tokens: Vec<Address> = self.edges.keys().copied().collect();
        let n = tokens.len();
        let idx: HashMap<Address, usize> = tokens.iter().enumerate().map(|(i, &t)| (t, i)).collect();

        // Initialize distances
        let mut dist = vec![f64::INFINITY; n];
        let mut prev: Vec<Option<(usize, Address)>> = vec![None; n]; // (from_token_idx, pool)

        // Run Bellman-Ford from each source
        // (Full negative-cycle detection)
        let mut cycle_starts: Vec<Address> = Vec::new();

        for &source in &tokens {
            let src_idx = idx[&source];
            dist.fill(f64::INFINITY);
            dist[src_idx] = 0.0;

            // Relax all edges (n-1) times
            for _ in 0..(n - 1) {
                for (&token_in, out_edges) in &self.edges {
                    let u = idx[&token_in];
                    for edge in out_edges {
                        let v = idx[&edge.token_out];
                        let rate = edge.effective_rate_per_unit();
                        if rate <= 0.0 { continue; }
                        let w = -rate.ln(); // negative weight = profitable direction
                        if dist[u] + w < dist[v] {
                            dist[v] = dist[u] + w;
                        }
                    }
                }
            }

            // n-th relaxation: if any distance improves, negative cycle exists
            for (&token_in, out_edges) in &self.edges {
                let u = idx[&token_in];
                for edge in out_edges {
                    let v = idx[&edge.token_out];
                    let rate = edge.effective_rate_per_unit();
                    if rate <= 0.0 { continue; }
                    let w = -rate.ln();
                    if dist[u] + w < dist[v] {
                        cycle_starts.push(token_in);
                    }
                }
            }
        }

        // Reconstruct 3-hop cycles from detected start tokens
        // Fall back to limited DFS for cycle reconstruction
        let mut routes = Vec::new();
        for start in cycle_starts {
            let mut path = Vec::new();
            let mut visited = vec![start];
            self.dfs_recurse(start, start, &mut path, &mut visited, 3, &mut routes);
        }
        routes.sort_by(|a, b| {
            self.rates_product(&b.hops)
                .partial_cmp(&self.rates_product(&a.hops))
                .unwrap()
        });
        routes.dedup_by(|a, b| a.hops.len() == b.hops.len() && a.start_token == b.start_token);
        routes
    }

    /// Product of exchange rates across a route. > 1.0 means profitable before fees/gas.
    fn rates_product(&self, hops: &[RouteHop]) -> f64 {
        let mut product = 1.0_f64;
        for hop in hops {
            if let Some(edges) = self.edges.get(&hop.token_in) {
                if let Some(edge) = edges.iter().find(|e| e.pool == hop.pool && e.token_out == hop.token_out) {
                    let r = edge.effective_rate_per_unit();
                    if r <= 0.0 { return 0.0; }
                    product *= r;
                } else {
                    return 0.0;
                }
            } else {
                return 0.0;
            }
        }
        product
    }

    /// Evaluate a candidate route with GSS sizing.
    /// Returns None if the route is unprofitable after fees and gas.
    pub fn evaluate_route(
        &self,
        route:               &ArbRoute,
        state:               &Arc<BotState>,
        config:              &SkuaConfig,
        flash_available_usd: f64,
        token_price_usd:     f64,
    ) -> Option<SizingResult> {
        // Must be exactly 3 hops for S3
        if route.hops.len() != 3 { return None; }

        let pool_states = state.pool_states.read();

        let make_amm = |hop: &RouteHop| -> Option<AmmPool> {
            let ps = pool_states.get(&hop.pool)?;
            // Orient the pool correctly: token_in is reserve_in
            let (reserve_in, reserve_out) = if ps.token_a == hop.token_in {
                (ps.reserve_a as f64, ps.reserve_b as f64)
            } else {
                (ps.reserve_b as f64, ps.reserve_a as f64)
            };
            Some(AmmPool {
                reserve_in,
                reserve_out,
                fee_bps: ps.fee_bps as f64,
                amp: ps.amp,
                n_tokens: 2,
            })
        };

        let pool_ab = make_amm(&route.hops[0])?;
        let pool_bc = make_amm(&route.hops[1])?;
        let pool_ca = make_amm(&route.hops[2])?;

        let flash_fee_bps  = state.balancer_fee_bps.load(Ordering::Relaxed) as f64;
        let base_fee_wei   = state.base_fee_wei();
        let hype_usd       = state.hype_price_f64();

        // Snapshot pools for GSS closure (avoids holding RwLock across iterations)
        let (p_ab, p_bc, p_ca) = (pool_ab, pool_bc, pool_ca);

        let profit_fn = move |x: f64| -> f64 {
            net_profit_three_leg(
                &p_ab, &p_bc, &p_ca,
                x,
                flash_fee_bps,
                S3_TRI_ARB_3HOP_GAS_ESTIMATE,
                base_fee_wei,
                hype_usd,
            )
        };

        // Reserve USD values for Stage 1 imbalance probe (audit fix #12)
        let r_in_usd  = pools.get(0).map(|p| p.reserve_in  / 1e6).unwrap_or(1_000_000.0);
        let r_out_usd = pools.get(0).map(|p| p.reserve_out / 1e6).unwrap_or(1_000_000.0);
        let amp_entry = pools.get(0).map(|p| p.amp).unwrap_or(0.0);
        let amp_exit  = pools.get(2).map(|p| p.amp).unwrap_or(0.0);

        optimal_borrow_size(
            profit_fn,
            flash_available_usd,
            config.hard_cap_usd,
            token_price_usd,
            route.start_token_decimals,
            FlashProvider::Balancer,
            r_in_usd,
            r_out_usd,
            amp_entry,
            amp_exit,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rates_product_no_arb() {
        // No graph to test directly but verify AmmPool math embedded in rate calc
        let edge = PoolEdge {
            pool: Address::ZERO,
            token_out: Address::ZERO,
            fee_bps: 30.0,
            reserve_in: 1_000_000.0,
            reserve_out: 1_000_000.0,
            amp: 0.0,
            is_stable: false,
        };
        // Rate < 1 due to fee
        assert!(edge.effective_rate_per_unit() < 1.0);
    }
}
