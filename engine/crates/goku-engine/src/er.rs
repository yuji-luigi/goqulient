//! Port of GokuCore's `ERLayout` (`ERLayout.swift` in the Goku app): deterministic layered ("Sugiyama-lite") layout.
//! Parents go in the left-most layer and children to the right, per connected component; components are then
//! shelf-packed. It must give exactly the same numbers as Swift (parity/er-layout), so keep the arithmetic in the
//! same order as the Swift code when changing either.
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Size {
    pub width: f64,
    pub height: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    pub origin: Point,
    pub size: Size,
}

impl Rect {
    pub fn new(x: f64, y: f64, width: f64, height: f64) -> Self {
        Self { origin: Point { x, y }, size: Size { width, height } }
    }
    pub fn intersects(&self, o: &Rect) -> bool {
        let (ax, ay, bx, by) = (self.origin.x, self.origin.y, o.origin.x, o.origin.y);
        ax < bx + o.size.width && bx < ax + self.size.width && ay < by + o.size.height && by < ay + self.size.height
    }
}

/// Child → parent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Edge {
    pub child: String,
    pub parent: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Options {
    pub horizontal_gap: f64,
    pub vertical_gap: f64,
    pub component_gap: f64,
    pub ordering_sweeps: usize,
}

impl Default for Options {
    fn default() -> Self {
        Self { horizontal_gap: 90.0, vertical_gap: 36.0, component_gap: 80.0, ordering_sweeps: 6 }
    }
}

struct Placed {
    rects: Vec<(usize, Rect)>,
    width: f64,
    height: f64,
}

/// Entities without a size are left out. Unlike Swift (which traps), a repeated entity id is laid out once.
pub fn layout(
    entities: &[String],
    sizes: &HashMap<String, Size>,
    edges: &[Edge],
    options: &Options,
) -> BTreeMap<String, Rect> {
    let mut seen = HashSet::new();
    let ids: Vec<&str> =
        entities.iter().map(String::as_str).filter(|e| sizes.contains_key(*e) && seen.insert(*e)).collect();
    if ids.is_empty() {
        return BTreeMap::new();
    }
    let index: HashMap<&str, usize> = ids.iter().enumerate().map(|(i, id)| (*id, i)).collect();
    let n = ids.len();
    // Directed child -> parent adjacency, without self-loops or duplicates.
    let mut out = vec![BTreeSet::new(); n];
    let mut undirected = vec![BTreeSet::new(); n];
    for e in edges {
        let (Some(&c), Some(&p)) = (index.get(e.child.as_str()), index.get(e.parent.as_str())) else { continue };
        if c == p {
            continue;
        }
        out[c].insert(p);
        undirected[c].insert(p);
        undirected[p].insert(c);
    }

    // Connected components (BFS over sorted neighbours, stable order).
    let mut component = vec![usize::MAX; n];
    let mut components: Vec<Vec<usize>> = Vec::new();
    for start in 0..n {
        if component[start] != usize::MAX {
            continue;
        }
        let mut queue = vec![start];
        let mut head = 0;
        component[start] = components.len();
        while head < queue.len() {
            let v = queue[head];
            head += 1;
            for &w in &undirected[v] {
                if component[w] == usize::MAX {
                    component[w] = components.len();
                    queue.push(w);
                }
            }
        }
        queue.sort_unstable();
        components.push(queue);
    }

    let size_of = |v: usize| sizes[ids[v]];
    let mut placed: Vec<Placed> = Vec::new();
    let mut singletons = Vec::new();
    for comp in &components {
        if comp.len() == 1 && out[comp[0]].is_empty() {
            singletons.push(comp[0]);
            continue;
        }
        placed.push(layout_component(comp, &ids, &size_of, &out, options));
    }
    if !singletons.is_empty() {
        placed.push(grid(&singletons, &size_of, options));
    }

    // Shelf packing: biggest components first (stable for equal areas, like Swift), rows near sqrt(total area) wide.
    let mut order: Vec<usize> = (0..placed.len()).collect();
    order.sort_by(|&a, &b| {
        let (aa, ab) = (placed[a].width * placed[a].height, placed[b].width * placed[b].height);
        ab.partial_cmp(&aa).unwrap_or(std::cmp::Ordering::Equal)
    });
    let total_area =
        placed.iter().fold(0.0, |acc, c| acc + (c.width + options.component_gap) * (c.height + options.component_gap));
    let widest = placed.iter().map(|c| c.width).fold(None, |m: Option<f64>, w| Some(m.map_or(w, |m| m.max(w))));
    let max_row_width = widest.unwrap_or(0.0).max(total_area.sqrt() * 1.6);
    let mut result = BTreeMap::new();
    let (mut x, mut y, mut row_height) = (0.0, 0.0, 0.0_f64);
    for i in order {
        let c = &placed[i];
        if x > 0.0 && x + c.width > max_row_width {
            x = 0.0;
            y += row_height + options.component_gap;
            row_height = 0.0;
        }
        for &(v, r) in &c.rects {
            result.insert(ids[v].to_owned(), Rect::new(r.origin.x + x, r.origin.y + y, r.size.width, r.size.height));
        }
        x += c.width + options.component_gap;
        row_height = row_height.max(c.height);
    }
    result
}

fn layout_component(
    comp: &[usize],
    ids: &[&str],
    size_of: &impl Fn(usize) -> Size,
    out: &[BTreeSet<usize>],
    options: &Options,
) -> Placed {
    let members: HashSet<usize> = comp.iter().copied().collect();
    let successors = |v: usize| -> Vec<usize> { out[v].iter().copied().filter(|w| members.contains(w)).collect() };
    // Break cycles: DFS, dropping back edges.
    const ON_STACK: u8 = 1;
    const DONE: u8 = 2;
    let mut state: HashMap<usize, u8> = HashMap::new();
    let mut dag: HashMap<usize, Vec<usize>> = HashMap::new();
    for &root in comp {
        if state.contains_key(&root) {
            continue;
        }
        let mut stack = vec![(root, successors(root), 0usize)];
        state.insert(root, ON_STACK);
        while let Some(top) = stack.last_mut() {
            let (v, i) = (top.0, top.2);
            if i < top.1.len() {
                top.2 += 1;
                let w = top.1[i];
                if state.get(&w) == Some(&ON_STACK) {
                    continue; // back edge -> ignore
                }
                dag.entry(v).or_default().push(w);
                if let std::collections::hash_map::Entry::Vacant(e) = state.entry(w) {
                    e.insert(ON_STACK);
                    stack.push((w, successors(w), 0));
                }
            } else {
                state.insert(v, DONE);
                stack.pop();
            }
        }
    }
    // Layer = longest path to a sink (parents end up in layer 0), computed in reverse topological order.
    let no_succ: Vec<usize> = Vec::new();
    let succ = |v: usize| dag.get(&v).unwrap_or(&no_succ);
    let mut visited = HashSet::new();
    let mut topo = Vec::new();
    for &root in comp {
        if !visited.insert(root) {
            continue;
        }
        let mut stack = vec![(root, 0usize)];
        while let Some(top) = stack.last_mut() {
            let (v, i) = *top;
            if i < succ(v).len() {
                top.1 += 1;
                let w = succ(v)[i];
                if visited.insert(w) {
                    stack.push((w, 0));
                }
            } else {
                topo.push(v);
                stack.pop();
            }
        }
    }
    let mut layer: HashMap<usize, usize> = HashMap::new();
    for &v in &topo {
        let l = succ(v).iter().map(|w| layer.get(w).copied().unwrap_or(0) + 1).max().unwrap_or(0);
        layer.insert(v, l);
    }
    let layer_count = layer.values().copied().max().unwrap_or(0) + 1;
    let mut layers: Vec<Vec<usize>> = vec![Vec::new(); layer_count];
    for &v in comp {
        layers[layer.get(&v).copied().unwrap_or(0)].push(v);
    }

    // Undirected neighbours within the component for barycenter ordering.
    let mut neighbours: HashMap<usize, Vec<usize>> = HashMap::new();
    for (&v, ws) in &dag {
        for &w in ws {
            neighbours.entry(v).or_default().push(w);
            neighbours.entry(w).or_default().push(v);
        }
    }
    let mut position: HashMap<usize, f64> = HashMap::new();
    let reindex = |layers: &[Vec<usize>], position: &mut HashMap<usize, f64>| {
        for l in layers {
            for (i, &v) in l.iter().enumerate() {
                position.insert(v, i as f64);
            }
        }
    };
    reindex(&layers, &mut position);
    for sweep in 0..options.ordering_sweeps {
        let range: Vec<usize> = if sweep % 2 == 0 {
            (1..layer_count.max(1)).collect()
        } else {
            (0..layer_count.saturating_sub(1)).rev().collect()
        };
        for li in range {
            let reference = if sweep % 2 == 0 { li as isize - 1 } else { li as isize + 1 };
            if reference < 0 || reference >= layer_count as isize {
                continue;
            }
            let ref_set: HashSet<usize> = layers[reference as usize].iter().copied().collect();
            let bary = |v: usize| -> f64 {
                let ns: Vec<usize> = neighbours
                    .get(&v)
                    .map(|ns| ns.iter().copied().filter(|n| ref_set.contains(n)).collect())
                    .unwrap_or_default();
                if ns.is_empty() {
                    position.get(&v).copied().unwrap_or(0.0)
                } else {
                    ns.iter().fold(0.0, |acc, n| acc + position.get(n).copied().unwrap_or(0.0)) / ns.len() as f64
                }
            };
            let mut keyed: Vec<(f64, usize)> = layers[li].iter().map(|&v| (bary(v), v)).collect();
            keyed.sort_by(|(ba, a), (bb, b)| {
                if ba == bb { ids[*a].cmp(ids[*b]) } else { ba.partial_cmp(bb).unwrap_or(std::cmp::Ordering::Equal) }
            });
            layers[li] = keyed.into_iter().map(|(_, v)| v).collect();
            reindex(&layers, &mut position);
        }
    }

    // Coordinates: columns by layer, each column vertically centred.
    let column_heights: Vec<f64> = layers
        .iter()
        .map(|l| {
            l.iter().fold(0.0, |acc, &v| acc + size_of(v).height)
                + l.len().saturating_sub(1) as f64 * options.vertical_gap
        })
        .collect();
    let height =
        column_heights.iter().copied().fold(None, |m: Option<f64>, h| Some(m.map_or(h, |m| m.max(h)))).unwrap_or(0.0);
    let mut rects = Vec::new();
    let mut x = 0.0;
    for (li, l) in layers.iter().enumerate() {
        let width = l.iter().map(|&v| size_of(v).width).fold(None, |m: Option<f64>, w| Some(m.map_or(w, |m| m.max(w))));
        let mut y = (height - column_heights[li]) / 2.0;
        for &v in l {
            let s = size_of(v);
            rects.push((v, Rect::new(x, y, s.width, s.height)));
            y += s.height + options.vertical_gap;
        }
        x += width.unwrap_or(0.0) + options.horizontal_gap;
    }
    Placed { rects, width: (x - options.horizontal_gap).max(0.0), height }
}

fn grid(nodes: &[usize], size_of: &impl Fn(usize) -> Size, options: &Options) -> Placed {
    let columns = ((nodes.len() as f64).sqrt().ceil() as usize).max(1);
    let mut col_widths = vec![0.0_f64; columns];
    for (i, &v) in nodes.iter().enumerate() {
        col_widths[i % columns] = col_widths[i % columns].max(size_of(v).width);
    }
    let (mut y, mut row_height, mut max_x) = (0.0, 0.0_f64, 0.0_f64);
    let mut rects = Vec::new();
    for (i, &v) in nodes.iter().enumerate() {
        let c = i % columns;
        if c == 0 && i > 0 {
            y += row_height + options.vertical_gap;
            row_height = 0.0;
        }
        let x = col_widths[..c].iter().fold(0.0, |acc, w| acc + w) + c as f64 * options.horizontal_gap / 2.0;
        let s = size_of(v);
        rects.push((v, Rect::new(x, y, s.width, s.height)));
        row_height = row_height.max(s.height);
        max_x = max_x.max(x + s.width);
    }
    Placed { rects, width: max_x, height: y + row_height }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sized(ids: &[&str]) -> HashMap<String, Size> {
        ids.iter().map(|id| (id.to_string(), Size { width: 100.0, height: 50.0 })).collect()
    }

    #[test]
    fn parents_left_of_children_no_overlaps() {
        let ids: Vec<String> = ["orders", "customers", "items", "products"].map(String::from).to_vec();
        let edges = [("orders", "customers"), ("items", "orders"), ("items", "products")]
            .map(|(c, p)| Edge { child: c.into(), parent: p.into() });
        let r = layout(&ids, &sized(&["orders", "customers", "items", "products"]), &edges, &Options::default());
        assert!(r["customers"].origin.x < r["orders"].origin.x && r["orders"].origin.x < r["items"].origin.x);
        let rects: Vec<_> = r.values().collect();
        for (i, a) in rects.iter().enumerate() {
            for b in &rects[i + 1..] {
                assert!(!a.intersects(b));
            }
        }
    }

    #[test]
    fn duplicates_and_unsized_entities_are_tolerated() {
        let ids: Vec<String> = ["a", "a", "b"].map(String::from).to_vec();
        let r = layout(&ids, &sized(&["a"]), &[], &Options::default());
        assert_eq!(r.keys().collect::<Vec<_>>(), ["a"]);
        assert!(layout(&[], &sized(&[]), &[], &Options::default()).is_empty());
    }
}
