use std::{
    collections::{HashMap, HashSet, VecDeque},
    hash::Hash,
};

/// Output of [`graph_sequencer`].
///
/// Ports the `Result<T>` shape from
/// `https://github.com/pnpm/pnpm/blob/80037699fb/deps/graph-sequencer/src/index.ts`.
#[derive(Debug)]
pub struct GraphSequencerResult<T> {
    /// `false` when at least one cycle of length > 1 was found.
    pub safe: bool,
    /// Topologically ordered groups. Every node in chunk `i` has all of its
    /// outgoing edges (within the included subset) pointing into earlier
    /// chunks `0..i`, so chunk `i` may run only after chunks `0..i` finish.
    pub chunks: Vec<Vec<T>>,
    /// Cycles encountered while sorting. Each cycle is a list of nodes.
    pub cycles: Vec<Vec<T>>,
}

/// Topologically sort a graph into chunks.
///
/// Ports `graphSequencer` from
/// `https://github.com/pnpm/pnpm/blob/80037699fb/deps/graph-sequencer/src/index.ts`.
///
/// `graph` is a node → outgoing-edges map. `included` selects the subset of
/// nodes to be sorted. Edges to nodes outside the included set are ignored.
///
/// Iteration order follows `included`, so the output is deterministic for a
/// given input order.
pub fn graph_sequencer<T>(graph: &HashMap<T, Vec<T>>, included: &[T]) -> GraphSequencerResult<T>
where
    T: Eq + Hash + Clone,
{
    let mut reverse_graph: HashMap<T, Vec<T>> =
        graph.keys().map(|k| (k.clone(), Vec::new())).collect();

    let mut remaining: HashSet<T> = included.iter().cloned().collect();
    let mut visited: HashSet<T> = HashSet::new();
    let mut out_degree: HashMap<T, usize> = HashMap::new();

    for (from, edges) in graph {
        out_degree.insert(from.clone(), 0);
        for to in edges {
            if remaining.contains(from) && remaining.contains(to) {
                *out_degree.entry(from.clone()).or_insert(0) += 1;
                reverse_graph.entry(to.clone()).or_default().push(from.clone());
            }
        }
        if !remaining.contains(from) {
            visited.insert(from.clone());
        }
    }

    let mut chunks: Vec<Vec<T>> = Vec::new();
    let mut cycles: Vec<Vec<T>> = Vec::new();
    let mut safe = true;

    while !remaining.is_empty() {
        let mut chunk: Vec<T> = Vec::new();
        let mut min_degree: usize = usize::MAX;
        for node in included {
            if !remaining.contains(node) {
                continue;
            }
            let degree = *out_degree.get(node).unwrap_or(&0);
            if degree == 0 {
                chunk.push(node.clone());
            }
            if degree < min_degree {
                min_degree = degree;
            }
        }

        if min_degree == 0 {
            for node in &chunk {
                remove_node(node, &reverse_graph, &mut out_degree, &mut visited, &mut remaining);
            }
            chunks.push(chunk);
        } else {
            let mut cycle_nodes: Vec<T> = Vec::new();
            for node in included {
                if !remaining.contains(node) {
                    continue;
                }
                let cycle = find_cycle(node, graph, &visited);
                if cycle.is_empty() {
                    continue;
                }
                if cycle.len() > 1 {
                    safe = false;
                }
                for n in &cycle {
                    remove_node(n, &reverse_graph, &mut out_degree, &mut visited, &mut remaining);
                }
                cycle_nodes.extend(cycle.iter().cloned());
                cycles.push(cycle);
            }
            chunks.push(cycle_nodes);
        }
    }

    GraphSequencerResult { safe, chunks, cycles }
}

fn remove_node<T>(
    node: &T,
    reverse_graph: &HashMap<T, Vec<T>>,
    out_degree: &mut HashMap<T, usize>,
    visited: &mut HashSet<T>,
    remaining: &mut HashSet<T>,
) where
    T: Eq + Hash + Clone,
{
    if let Some(parents) = reverse_graph.get(node) {
        for parent in parents {
            if let Some(deg) = out_degree.get_mut(parent)
                && *deg > 0
            {
                *deg -= 1;
            }
        }
    }
    visited.insert(node.clone());
    remaining.remove(node);
}

fn find_cycle<T>(start: &T, graph: &HashMap<T, Vec<T>>, visited: &HashSet<T>) -> Vec<T>
where
    T: Eq + Hash + Clone,
{
    let mut queue: VecDeque<(T, Vec<T>)> = VecDeque::new();
    queue.push_back((start.clone(), vec![start.clone()]));
    let mut cycle_visited: HashSet<T> = HashSet::new();
    let mut found_cycles: Vec<Vec<T>> = Vec::new();

    while let Some((id, cycle)) = queue.pop_front() {
        let Some(edges) = graph.get(&id) else { continue };
        for to in edges {
            if to == start {
                cycle_visited.insert(to.clone());
                found_cycles.push(cycle.clone());
                continue;
            }
            if visited.contains(to) || cycle_visited.contains(to) {
                continue;
            }
            cycle_visited.insert(to.clone());
            let mut new_cycle = cycle.clone();
            new_cycle.push(to.clone());
            queue.push_back((to.clone(), new_cycle));
        }
    }

    found_cycles.sort_by_key(|c| std::cmp::Reverse(c.len()));
    found_cycles.into_iter().next().unwrap_or_default()
}

#[cfg(test)]
mod tests;
