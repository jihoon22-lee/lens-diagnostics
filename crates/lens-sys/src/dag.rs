use crate::model::{OrderingEdge, SystemdUnit};
use crate::parser::{instantiate_template, is_template_name, template_name_of};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

/// An ordering constraint `before` must start before `after`, produced
/// by a `Before=`/`After=` directive declared at `path:line`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OrderingConstraint {
    pub before: String,
    pub after: String,
    /// `Before` or `After`, as written.
    pub directive: String,
    /// The unit that declared the directive.
    pub declared_by: String,
    pub path: Option<String>,
    pub line: Option<usize>,
}

/// A directed cycle in the ordering graph: `members` is the SCC and
/// `edges` is one concrete directed cycle path annotated with the
/// directive (and file:line) responsible for each hop.
#[derive(Debug, Clone, Serialize)]
pub struct CyclePath {
    pub members: Vec<String>,
    pub edges: Vec<OrderingConstraint>,
}

#[derive(Debug, Clone, Default)]
pub struct OrderingGraph {
    pub adj: BTreeMap<String, BTreeSet<String>>,
    pub all_nodes: BTreeSet<String>,
    /// Every ordering constraint with its provenance.
    pub constraints: Vec<OrderingConstraint>,
}

impl OrderingGraph {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn build(units: &BTreeMap<String, SystemdUnit>) -> Self {
        let mut graph = OrderingGraph::new();

        // Symlink aliases resolve to their canonical unit; masked,
        // alias, and template units never join the ordering graph directly.
        let alias_map: BTreeMap<&str, &str> = units
            .iter()
            .filter_map(|(n, u)| u.alias_of.as_deref().map(|t| (n.as_str(), t)))
            .collect();
        let resolve = |name: &str| -> String {
            alias_map
                .get(name)
                .map(|t| t.to_string())
                .unwrap_or_else(|| name.to_string())
        };
        let excluded = |name: &str| -> bool {
            if is_template_name(name) {
                return true;
            }
            if let Some(u) = units.get(name) {
                return u.masked || u.alias_of.is_some();
            }
            if let Some(tpl_name) = template_name_of(name) {
                if let Some(tpl) = units.get(&tpl_name) {
                    return tpl.masked || tpl.alias_of.is_some();
                }
            }
            false
        };

        // Collect concrete template instances (e.g. `foo@bar.service`)
        // referenced by loaded units when `foo@bar.service` is not explicitly
        // in `units` but its template `foo@.service` is.
        let mut synthesized_instances: BTreeMap<String, SystemdUnit> = BTreeMap::new();
        for (name, unit) in units {
            if excluded(name) {
                continue;
            }
            let refs = unit
                .before
                .iter()
                .chain(unit.after.iter())
                .chain(unit.wants.iter())
                .chain(unit.requires.iter())
                .chain(unit.ordering_edges.iter().map(|e| &e.target));
            for raw_target in refs {
                let target = resolve(raw_target);
                if units.contains_key(&target) || synthesized_instances.contains_key(&target) {
                    continue;
                }
                if let Some(tpl_name) = template_name_of(&target) {
                    if let Some(tpl_unit) = units.get(&tpl_name) {
                        if !tpl_unit.masked && tpl_unit.alias_of.is_none() {
                            synthesized_instances
                                .insert(target.clone(), instantiate_template(tpl_unit, &target));
                        }
                    }
                }
            }
        }

        for (name, unit) in units.iter().chain(synthesized_instances.iter()) {
            if excluded(name) {
                continue;
            }
            graph.all_nodes.insert(name.clone());
            graph.adj.entry(name.clone()).or_default();

            // Prefer per-line ordering edges; fall back to the flattened
            // lists for units built without provenance (decoded snapshots,
            // hand-constructed units).
            let declared: Vec<OrderingEdge> = if !unit.ordering_edges.is_empty() {
                unit.ordering_edges.clone()
            } else {
                let mk = |t: &String, directive: &str| OrderingEdge {
                    directive: directive.to_string(),
                    source: name.clone(),
                    target: t.clone(),
                    path: unit.path.clone(),
                    line: None,
                };
                unit.before
                    .iter()
                    .map(|t| mk(t, "Before"))
                    .chain(unit.after.iter().map(|t| mk(t, "After")))
                    .collect()
            };

            for e in declared {
                let target = resolve(&e.target);
                if excluded(&target) {
                    continue;
                }
                graph.all_nodes.insert(target.clone());
                graph.adj.entry(target.clone()).or_default();
                // A has Before=T => A before T => edge A -> T;
                // A has After=T  => T before A => edge T -> A.
                let (before, after) = if e.directive == "After" {
                    (target, name.clone())
                } else {
                    (name.clone(), target)
                };
                graph
                    .adj
                    .entry(before.clone())
                    .or_default()
                    .insert(after.clone());
                graph.constraints.push(OrderingConstraint {
                    before,
                    after,
                    directive: e.directive,
                    declared_by: e.source,
                    path: e.path.or_else(|| unit.path.clone()),
                    line: e.line,
                });
            }
        }

        graph
    }

    /// Detect cycles in the ordering graph using Tarjan's Strongly Connected Components algorithm.
    pub fn find_cycles(&self) -> Vec<Vec<String>> {
        let mut index = 0usize;
        let mut stack = Vec::new();
        let mut indices: BTreeMap<String, usize> = BTreeMap::new();
        let mut lowlink: BTreeMap<String, usize> = BTreeMap::new();
        let mut on_stack: BTreeSet<String> = BTreeSet::new();
        let mut sccs: Vec<Vec<String>> = Vec::new();

        for node in &self.all_nodes {
            if !indices.contains_key(node) {
                strongconnect(
                    node,
                    &self.adj,
                    &mut index,
                    &mut stack,
                    &mut indices,
                    &mut lowlink,
                    &mut on_stack,
                    &mut sccs,
                );
            }
        }

        // Filter SCCs: only keep those with > 1 node, or a single node with a self-loop
        let mut cycles = Vec::new();
        for mut scc in sccs {
            if scc.len() > 1 {
                scc.sort();
                cycles.push(scc);
            } else if let Some(single) = scc.first() {
                if let Some(neighbors) = self.adj.get(single) {
                    if neighbors.contains(single) {
                        cycles.push(scc);
                    }
                }
            }
        }

        cycles.sort();
        cycles
    }

    /// Each SCC plus one concrete directed cycle path through it, with
    /// the directive and file:line responsible for every hop.
    pub fn find_cycle_paths(&self) -> Vec<CyclePath> {
        self.find_cycles()
            .into_iter()
            .map(|members| {
                let member_set: BTreeSet<&str> = members.iter().map(|m| m.as_str()).collect();
                CyclePath {
                    edges: extract_cycle(&member_set, &self.constraints),
                    members,
                }
            })
            .collect()
    }

    /// Perform a topological sort on DAG (if acyclic).
    pub fn topological_sort(&self) -> Option<Vec<String>> {
        let mut in_degree: BTreeMap<String, usize> = BTreeMap::new();
        for node in &self.all_nodes {
            in_degree.insert(node.clone(), 0);
        }

        for targets in self.adj.values() {
            for t in targets {
                *in_degree.entry(t.clone()).or_default() += 1;
            }
        }

        let mut queue: Vec<String> = in_degree
            .iter()
            .filter(|(_, &deg)| deg == 0)
            .map(|(n, _)| n.clone())
            .collect();
        queue.sort();

        let mut order = Vec::new();

        while let Some(node) = queue.pop() {
            order.push(node.clone());
            if let Some(neighbors) = self.adj.get(&node) {
                for next in neighbors {
                    if let Some(deg) = in_degree.get_mut(next) {
                        *deg -= 1;
                        if *deg == 0 {
                            queue.push(next.clone());
                            queue.sort();
                        }
                    }
                }
            }
        }

        if order.len() == self.all_nodes.len() {
            Some(order)
        } else {
            None // Cycle exists
        }
    }
}

/// Find one directed cycle covering a strongly-connected component by
/// DFS over the ordering constraints restricted to the SCC. Every SCC
/// member has at least one outgoing edge inside the SCC, so a cycle
/// always exists.
fn extract_cycle(
    scc: &BTreeSet<&str>,
    constraints: &[OrderingConstraint],
) -> Vec<OrderingConstraint> {
    let mut adj: BTreeMap<&str, Vec<&OrderingConstraint>> = BTreeMap::new();
    for c in constraints {
        if scc.contains(c.before.as_str()) && scc.contains(c.after.as_str()) {
            adj.entry(c.before.as_str()).or_default().push(c);
        }
    }
    for edges in adj.values_mut() {
        edges.sort_by(|a, b| {
            (&a.after, &a.directive, &a.path, &a.line).cmp(&(
                &b.after,
                &b.directive,
                &b.path,
                &b.line,
            ))
        });
    }

    let Some(start) = scc.iter().next().copied() else {
        return Vec::new();
    };
    let mut path: Vec<&str> = vec![start];
    let mut path_edges: Vec<&OrderingConstraint> = Vec::new();
    let mut on_path: BTreeSet<&str> = BTreeSet::from([start]);
    let mut explored: BTreeSet<&str> = BTreeSet::new();

    fn dfs<'a>(
        node: &'a str,
        adj: &BTreeMap<&'a str, Vec<&'a OrderingConstraint>>,
        path: &mut Vec<&'a str>,
        path_edges: &mut Vec<&'a OrderingConstraint>,
        on_path: &mut BTreeSet<&'a str>,
        explored: &mut BTreeSet<&'a str>,
    ) -> Option<Vec<OrderingConstraint>> {
        if let Some(edges) = adj.get(node) {
            for e in edges {
                let next = e.after.as_str();
                if on_path.contains(next) {
                    // Back-edge into the active path: the cycle is the
                    // path slice from `next` plus this closing edge.
                    let idx = path.iter().position(|n| *n == next).unwrap_or(0);
                    let mut cycle: Vec<OrderingConstraint> =
                        path_edges[idx..].iter().map(|e| (*e).clone()).collect();
                    cycle.push((*e).clone());
                    return Some(cycle);
                }
                if explored.insert(next) {
                    path.push(next);
                    on_path.insert(next);
                    path_edges.push(e);
                    if let Some(cycle) = dfs(next, adj, path, path_edges, on_path, explored) {
                        return Some(cycle);
                    }
                    path.pop();
                    on_path.remove(next);
                    path_edges.pop();
                }
            }
        }
        None
    }

    dfs(
        start,
        &adj,
        &mut path,
        &mut path_edges,
        &mut on_path,
        &mut explored,
    )
    .unwrap_or_default()
}

#[allow(clippy::too_many_arguments)]
fn strongconnect(
    node: &str,
    adj: &BTreeMap<String, BTreeSet<String>>,
    index: &mut usize,
    stack: &mut Vec<String>,
    indices: &mut BTreeMap<String, usize>,
    lowlink: &mut BTreeMap<String, usize>,
    on_stack: &mut BTreeSet<String>,
    sccs: &mut Vec<Vec<String>>,
) {
    indices.insert(node.to_string(), *index);
    lowlink.insert(node.to_string(), *index);
    *index += 1;

    stack.push(node.to_string());
    on_stack.insert(node.to_string());

    if let Some(neighbors) = adj.get(node) {
        for next in neighbors {
            if !indices.contains_key(next) {
                strongconnect(next, adj, index, stack, indices, lowlink, on_stack, sccs);
                let next_low = lowlink[next];
                let node_low = lowlink.get_mut(node).unwrap();
                *node_low = (*node_low).min(next_low);
            } else if on_stack.contains(next) {
                let next_idx = indices[next];
                let node_low = lowlink.get_mut(node).unwrap();
                *node_low = (*node_low).min(next_idx);
            }
        }
    }

    if lowlink[node] == indices[node] {
        let mut scc = Vec::new();
        while let Some(w) = stack.pop() {
            on_stack.remove(&w);
            scc.push(w.clone());
            if w == node {
                break;
            }
        }
        sccs.push(scc);
    }
}
