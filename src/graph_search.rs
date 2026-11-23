//! `control.search.path`: shortest paths over a graph or grid the caller
//! supplies inline. A* with a consistent heuristic when one is available,
//! Dijkstra otherwise; both are one best-first loop. Pure, bounded and
//! stateless: nothing is stored and no host resource is touched.
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    cmp::Ordering,
    collections::{BinaryHeap, HashMap},
    io,
    time::{Duration, Instant},
};

const MAX_NODES: usize = 20_000;
const MAX_EDGES: usize = 100_000;
const MAX_CELLS: usize = 1_000_000;
const MAX_GOALS: usize = 256;
const MAX_EXPANSIONS: u64 = 2_000_000;
const DEADLINE: Duration = Duration::from_secs(2);
/// Rounding slack for cost comparisons.
const EPSILON: f64 = 1e-9;

pub fn definition() -> Value {
    json!({
        "name": "control.search.path",
        "description": concat!(
            "Shortest path by A* (Dijkstra without a heuristic) over a graph or grid you supply inline. Use it for ",
            "routing, navigation, move or dependency search instead of reasoning paths out in tokens. Graph: nodes ",
            "(ids, or {id, at:[x,y(,z)], h}), edges [from,to,cost?] (cost defaults to 1; undirected unless ",
            "directed=true). heuristic auto uses explicit h values, else coordinates (Euclidean, scaled down if an ",
            "edge is cheaper than its length, so results stay optimal), else none. Grid: size [w,h(,d)] and/or rows ",
            "of text ('#' blocked, digits 1-9 cell cost, else 1), blocked cells, flat costs (x fastest), moves 4|8 ",
            "(2D) or 6|26 (3D); entering a cell costs its cost times the step length, and diagonals never cut blocked ",
            "corners. start, goal or goals (nearest wins), optional avoid (nodes or cells), avoid_edges, max_cost, ",
            "max_expansions. Limits: 20000 nodes, 100000 edges, 1000000 cells, 256 goals, 2000000 expansions, 2 s. ",
            "Returns path, cost, expanded count."
        ),
        "annotations": {"readOnlyHint": true, "openWorldHint": false},
        "inputSchema": {
            "type": "object",
            "properties": {
                "nodes": {"type": "array", "maxItems": MAX_NODES},
                "edges": {"type": "array", "maxItems": MAX_EDGES},
                "directed": {"type": "boolean", "default": false},
                "grid": {
                    "type": "object",
                    "properties": {
                        "size": {
                            "type": "array",
                            "items": {"type": "integer", "minimum": 1},
                            "minItems": 2,
                            "maxItems": 3
                        },
                        "rows": {"type": "array", "items": {"type": "string"}},
                        "blocked": {"type": "array", "items": {"type": "array", "items": {"type": "integer"}}},
                        "costs": {"type": "array", "items": {"type": "number", "minimum": 0}},
                        "moves": {"type": "integer", "enum": [4, 8, 6, 26]}
                    },
                    "additionalProperties": false
                },
                "start": {},
                "goal": {},
                "goals": {"type": "array", "maxItems": MAX_GOALS},
                "heuristic": {
                    "type": "string",
                    "enum": ["auto", "none", "euclidean", "manhattan", "explicit"],
                    "default": "auto"
                },
                "avoid": {"type": "array"},
                "avoid_edges": {"type": "array", "items": {"type": "array", "minItems": 2, "maxItems": 2}},
                "max_cost": {"type": "number", "minimum": 0},
                "max_expansions": {"type": "integer", "minimum": 1, "maximum": MAX_EXPANSIONS}
            },
            "required": ["start"],
            "additionalProperties": false
        }
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    #[serde(default)]
    nodes: Vec<NodeSpec>,
    #[serde(default)]
    edges: Vec<EdgeSpec>,
    #[serde(default)]
    directed: bool,
    #[serde(default)]
    grid: Option<GridSpec>,
    start: Value,
    #[serde(default)]
    goal: Option<Value>,
    #[serde(default)]
    goals: Vec<Value>,
    #[serde(default)]
    heuristic: Heuristic,
    #[serde(default)]
    avoid: Vec<Value>,
    #[serde(default)]
    avoid_edges: Vec<(Value, Value)>,
    #[serde(default)]
    max_cost: Option<f64>,
    #[serde(default)]
    max_expansions: Option<u64>,
}
#[derive(Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Heuristic {
    #[default]
    Auto,
    None,
    Euclidean,
    Manhattan,
    Explicit,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(untagged)]
enum Id {
    Number(i64),
    Text(String),
}
impl Id {
    fn key(&self) -> String {
        match self {
            Id::Number(number) => number.to_string(),
            Id::Text(text) => text.clone(),
        }
    }
}
#[derive(Deserialize)]
#[serde(untagged)]
enum NodeSpec {
    Bare(Id),
    Full(Node),
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Node {
    id: Id,
    #[serde(default)]
    at: Option<Vec<f64>>,
    #[serde(default)]
    h: Option<f64>,
}
#[derive(Deserialize)]
#[serde(untagged)]
enum EdgeSpec {
    Weighted(Id, Id, f64),
    Plain(Id, Id),
    Object(Edge),
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Edge {
    from: Id,
    to: Id,
    #[serde(default)]
    cost: Option<f64>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GridSpec {
    #[serde(default)]
    size: Option<Vec<usize>>,
    #[serde(default)]
    rows: Vec<String>,
    #[serde(default)]
    blocked: Vec<Vec<i64>>,
    #[serde(default)]
    costs: Vec<f64>,
    #[serde(default)]
    moves: Option<u8>,
}

pub fn call(args: &Value) -> io::Result<Value> {
    let input = Input::deserialize(args).map_err(|error| io::Error::other(format!("Invalid search input: {error}")))?;
    let limits = Limits {
        max_cost: input
            .max_cost
            .filter(|cost| cost.is_finite() && *cost >= 0.0)
            .map_or(f64::INFINITY, |cost| cost + EPSILON),
        expansions: input.max_expansions.unwrap_or(MAX_EXPANSIONS).clamp(1, MAX_EXPANSIONS),
        deadline: DEADLINE,
    };
    if input.max_cost.is_some_and(|cost| !cost.is_finite() || cost < 0.0) {
        return Err(io::Error::other("max_cost must be a non-negative number."));
    }
    let goals = input.goal.iter().chain(&input.goals).collect::<Vec<_>>();
    if goals.is_empty() || goals.len() > MAX_GOALS {
        return Err(io::Error::other(format!("Give goal or 1–{MAX_GOALS} goals.")));
    }
    match input.grid {
        Some(_) if !input.nodes.is_empty() || !input.edges.is_empty() || !input.avoid_edges.is_empty() => {
            Err(io::Error::other("Use either grid or nodes/edges, not both."))
        }
        Some(ref spec) => {
            if !matches!(input.heuristic, Heuristic::Auto | Heuristic::None) {
                return Err(io::Error::other(
                    "Grids choose their heuristic from moves; use heuristic auto or none.",
                ));
            }
            let grid = Grid::new(spec, &input.avoid)?;
            let start = grid.cell(&input.start)?;
            let goals = goals
                .into_iter()
                .map(|goal| grid.cell(goal))
                .collect::<io::Result<Vec<_>>>()?;
            let space = GridSpace::new(grid, &goals, input.heuristic == Heuristic::Auto);
            let label = space.label();
            let found = search(&space, start, &goals, &limits)?;
            Ok(report(found, label, None, None, |cell| json!(space.grid.coords(cell))))
        }
        None => {
            let graph = Graph::new(&input)?;
            let start = graph.index(&input.start)?;
            let goals = goals
                .into_iter()
                .map(|goal| graph.index(goal))
                .collect::<io::Result<Vec<_>>>()?;
            let space = graph.space(input.heuristic, &goals)?;
            let label = space.label;
            let consistent = space.consistent();
            let found = search(&space, start, &goals, &limits)?;
            Ok(report(
                found,
                label,
                space.scale.filter(|scale| *scale < 1.0),
                consistent,
                |node| json!(space.graph.ids[node]),
            ))
        }
    }
}
fn report(
    found: Found,
    heuristic: &str,
    scale: Option<f64>,
    consistent: Option<bool>,
    id: impl Fn(usize) -> Value,
) -> Value {
    let algorithm = if heuristic == "none" { "dijkstra" } else { "astar" };
    let mut value = match found.path {
        Some((cost, path)) => {
            json!({"found":true,"cost":cost,"steps":path.len() - 1,"goal":id(*path.last().unwrap()),"path":path.into_iter().map(id).collect::<Vec<_>>()})
        }
        None => json!({"found":false,"reason":if found.pruned { "no path within max_cost" } else { "unreachable" }}),
    };
    value["expanded"] = json!(found.expanded);
    value["algorithm"] = json!(algorithm);
    value["heuristic"] = json!(heuristic);
    if let Some(scale) = scale {
        value["heuristic_scale"] = json!(scale);
    }
    if let Some(consistent) = consistent {
        // Inconsistent explicit values are re-expanded; optimal only if admissible.
        value["consistent"] = json!(consistent);
    }
    value
}

/// A search space: successors with step costs, and a lower bound on the
/// remaining cost to the nearest goal.
trait Space {
    fn len(&self) -> usize;
    fn successors(&self, node: usize, out: &mut Vec<(usize, f64)>);
    fn estimate(&self, node: usize) -> f64;
}
struct Limits {
    max_cost: f64,
    expansions: u64,
    deadline: Duration,
}
struct Found {
    path: Option<(f64, Vec<usize>)>,
    expanded: u64,
    pruned: bool,
}
#[derive(Clone, Copy)]
struct Open {
    f: f64,
    g: f64,
    node: u32,
}
impl PartialEq for Open {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}
impl Eq for Open {}
impl PartialOrd for Open {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Open {
    /// Max-heap order: lowest f first, then the deepest (highest g).
    fn cmp(&self, other: &Self) -> Ordering {
        other.f.total_cmp(&self.f).then(self.g.total_cmp(&other.g))
    }
}
/// Best-first search with lazy deletion. A node whose cost improves is pushed
/// again, so an inconsistent but admissible estimate still returns an optimal
/// path (by re-expansion); a consistent one expands each node at most once.
fn search(space: &impl Space, start: usize, goals: &[usize], limits: &Limits) -> io::Result<Found> {
    let began = Instant::now();
    let n = space.len();
    let mut goal = vec![false; n];
    for &index in goals {
        goal[index] = true;
    }
    let mut cost = vec![f64::INFINITY; n];
    let mut parent = vec![u32::MAX; n];
    let mut open = BinaryHeap::new();
    let mut successors = Vec::new();
    let (mut expanded, mut pruned) = (0u64, false);
    cost[start] = 0.0;
    open.push(Open {
        f: space.estimate(start),
        g: 0.0,
        node: start as u32,
    });
    while let Some(Open { g, node, .. }) = open.pop() {
        let node = node as usize;
        if g > cost[node] {
            continue;
        }
        if goal[node] {
            let mut path = vec![node];
            while parent[*path.last().unwrap()] != u32::MAX {
                path.push(parent[*path.last().unwrap()] as usize);
            }
            path.reverse();
            return Ok(Found {
                path: Some((g, path)),
                expanded,
                pruned,
            });
        }
        expanded += 1;
        if expanded > limits.expansions {
            return Err(io::Error::other(format!(
                "Search stopped after {} expansions without reaching a goal; raise max_expansions (≤{MAX_EXPANSIONS}), add a heuristic or shrink the input.",
                limits.expansions
            )));
        }
        if expanded.is_multiple_of(4096) && began.elapsed() > limits.deadline {
            return Err(io::Error::other(format!(
                "Search stopped at the {} s limit after {expanded} expansions without reaching a goal.",
                limits.deadline.as_secs()
            )));
        }
        successors.clear();
        space.successors(node, &mut successors);
        for &(next, step) in &successors {
            let through = g + step;
            if through >= cost[next] {
                continue;
            }
            if through > limits.max_cost {
                pruned = true;
                continue;
            }
            cost[next] = through;
            parent[next] = node as u32;
            open.push(Open {
                f: through + space.estimate(next),
                g: through,
                node: next as u32,
            });
        }
    }
    Ok(Found {
        path: None,
        expanded,
        pruned,
    })
}

/// A graph in compressed sparse rows.
struct Graph {
    ids: Vec<Id>,
    index: HashMap<String, usize>,
    at: Vec<Option<Vec<f64>>>,
    h: Vec<Option<f64>>,
    offsets: Vec<u32>,
    targets: Vec<(u32, f64)>,
    blocked: Vec<bool>,
}
impl Graph {
    fn new(input: &Input) -> io::Result<Self> {
        if input.nodes.len() > MAX_NODES || input.edges.len() > MAX_EDGES {
            return Err(io::Error::other(format!(
                "Graphs are limited to {MAX_NODES} nodes and {MAX_EDGES} edges."
            )));
        }
        let mut graph = Graph {
            ids: Vec::new(),
            index: HashMap::new(),
            at: Vec::new(),
            h: Vec::new(),
            offsets: Vec::new(),
            targets: Vec::new(),
            blocked: Vec::new(),
        };
        for spec in &input.nodes {
            let (id, at, h) = match spec {
                NodeSpec::Bare(id) => (id, None, None),
                NodeSpec::Full(node) => (&node.id, node.at.clone(), node.h),
            };
            if at
                .as_ref()
                .is_some_and(|at| at.is_empty() || at.len() > 3 || at.iter().any(|x| !x.is_finite()))
            {
                return Err(io::Error::other(format!(
                    "Node {} needs at as 1–3 finite coordinates.",
                    id.key()
                )));
            }
            if h.is_some_and(|h| !h.is_finite() || h < 0.0) {
                return Err(io::Error::other(format!(
                    "Node {} needs h as a non-negative number.",
                    id.key()
                )));
            }
            let index = graph.intern(id)?;
            if graph.at[index].is_some() || graph.h[index].is_some() {
                return Err(io::Error::other(format!("Node {} is listed twice.", id.key())));
            }
            (graph.at[index], graph.h[index]) = (at, h);
        }
        let mut avoided = std::collections::HashSet::new();
        for (from, to) in &input.avoid_edges {
            let (from, to) = (id(from)?.key(), id(to)?.key());
            if !input.directed {
                avoided.insert((to.clone(), from.clone()));
            }
            avoided.insert((from, to));
        }
        let mut edges = Vec::with_capacity(input.edges.len() * if input.directed { 1 } else { 2 });
        for spec in &input.edges {
            let (from, to, cost) = match spec {
                EdgeSpec::Weighted(from, to, cost) => (from, to, *cost),
                EdgeSpec::Plain(from, to) => (from, to, 1.0),
                EdgeSpec::Object(edge) => (&edge.from, &edge.to, edge.cost.unwrap_or(1.0)),
            };
            if !cost.is_finite() || cost < 0.0 {
                return Err(io::Error::other(format!(
                    "Edge {}→{} needs a non-negative finite cost.",
                    from.key(),
                    to.key()
                )));
            }
            let (a, b) = (graph.intern(from)?, graph.intern(to)?);
            if !avoided.is_empty() && avoided.contains(&(from.key(), to.key())) {
                continue;
            }
            edges.push((a as u32, b as u32, cost));
            if !input.directed {
                edges.push((b as u32, a as u32, cost));
            }
        }
        let n = graph.ids.len();
        graph.blocked = vec![false; n];
        for avoid in &input.avoid {
            let index = graph.index(avoid)?;
            graph.blocked[index] = true;
        }
        // Counting sort by source.
        graph.offsets = vec![0; n + 1];
        for &(from, ..) in &edges {
            graph.offsets[from as usize + 1] += 1;
        }
        for i in 0..n {
            graph.offsets[i + 1] += graph.offsets[i];
        }
        let mut fill = graph.offsets.clone();
        graph.targets = vec![(0, 0.0); edges.len()];
        for (from, to, cost) in edges {
            graph.targets[fill[from as usize] as usize] = (to, cost);
            fill[from as usize] += 1;
        }
        Ok(graph)
    }
    fn intern(&mut self, id: &Id) -> io::Result<usize> {
        let key = id.key();
        if let Some(&index) = self.index.get(&key) {
            return Ok(index);
        }
        if self.ids.len() == MAX_NODES {
            return Err(io::Error::other(format!("Graphs are limited to {MAX_NODES} nodes.")));
        }
        self.index.insert(key, self.ids.len());
        self.ids.push(id.clone());
        self.at.push(None);
        self.h.push(None);
        Ok(self.ids.len() - 1)
    }
    fn index(&self, value: &Value) -> io::Result<usize> {
        let key = id(value)?.key();
        self.index
            .get(&key)
            .copied()
            .ok_or_else(|| io::Error::other(format!("Unknown node {key}.")))
    }
    fn edges(&self) -> impl Iterator<Item = (usize, usize, f64)> + '_ {
        (0..self.ids.len()).flat_map(move |from| {
            self.targets[self.offsets[from] as usize..self.offsets[from + 1] as usize]
                .iter()
                .map(move |&(to, cost)| (from, to as usize, cost))
        })
    }
    fn space(&self, heuristic: Heuristic, goals: &[usize]) -> io::Result<GraphSpace<'_>> {
        let placed = self.at.iter().all(Option::is_some);
        let chosen = match heuristic {
            Heuristic::Auto if self.h.iter().any(Option::is_some) => Heuristic::Explicit,
            Heuristic::Auto if placed => Heuristic::Euclidean,
            Heuristic::Auto => Heuristic::None,
            other => other,
        };
        if matches!(chosen, Heuristic::Euclidean | Heuristic::Manhattan) {
            let missing = self.at.iter().position(Option::is_none);
            if let Some(index) = missing {
                return Err(io::Error::other(format!(
                    "A coordinate heuristic needs at on every node; {} has none.",
                    self.ids[index].key()
                )));
            }
            let dims = self.at[0].as_ref().map_or(0, Vec::len);
            if self.at.iter().flatten().any(|at| at.len() != dims) {
                return Err(io::Error::other(
                    "Every node's at needs the same number of coordinates.",
                ));
            }
        }
        let mut space = GraphSpace {
            graph: self,
            label: "none",
            goals: goals.to_vec(),
            scale: None,
            metric: None,
        };
        match chosen {
            Heuristic::None | Heuristic::Auto => {}
            Heuristic::Explicit => space.label = "explicit",
            Heuristic::Euclidean | Heuristic::Manhattan => {
                let metric = if chosen == Heuristic::Euclidean {
                    euclidean as Metric
                } else {
                    manhattan
                };
                // The largest factor that keeps the estimate a lower bound on
                // every edge, so it stays consistent (and A* optimal).
                let scale = self.edges().fold(1.0_f64, |scale, (from, to, cost)| {
                    let length = metric(self.at[from].as_deref().unwrap(), self.at[to].as_deref().unwrap());
                    if length > 0.0 { scale.min(cost / length) } else { scale }
                });
                space.scale = Some(scale);
                if scale > 0.0 {
                    space.metric = Some(metric);
                    space.label = if chosen == Heuristic::Euclidean {
                        "euclidean"
                    } else {
                        "manhattan"
                    };
                }
            }
        }
        Ok(space)
    }
}
fn id(value: &Value) -> io::Result<Id> {
    Id::deserialize(value).map_err(|_| io::Error::other("Node ids are strings or integers."))
}
type Metric = fn(&[f64], &[f64]) -> f64;
fn euclidean(a: &[f64], b: &[f64]) -> f64 {
    a.iter().zip(b).map(|(x, y)| (x - y) * (x - y)).sum::<f64>().sqrt()
}
fn manhattan(a: &[f64], b: &[f64]) -> f64 {
    a.iter().zip(b).map(|(x, y)| (x - y).abs()).sum()
}
struct GraphSpace<'a> {
    graph: &'a Graph,
    label: &'static str,
    goals: Vec<usize>,
    scale: Option<f64>,
    metric: Option<Metric>,
}
impl GraphSpace<'_> {
    /// Whether explicit estimates are consistent: zero at goals and never
    /// dropping by more than an edge's cost.
    fn consistent(&self) -> Option<bool> {
        (self.label == "explicit").then(|| {
            let h = |node: usize| self.graph.h[node].unwrap_or(0.0);
            self.goals.iter().all(|&goal| h(goal) == 0.0)
                && self
                    .graph
                    .edges()
                    .all(|(from, to, cost)| h(from) <= cost + h(to) + EPSILON)
        })
    }
}
impl Space for GraphSpace<'_> {
    fn len(&self) -> usize {
        self.graph.ids.len()
    }
    fn successors(&self, node: usize, out: &mut Vec<(usize, f64)>) {
        let graph = self.graph;
        out.extend(
            graph.targets[graph.offsets[node] as usize..graph.offsets[node + 1] as usize]
                .iter()
                .filter(|(to, _)| !graph.blocked[*to as usize])
                .map(|&(to, cost)| (to as usize, cost)),
        );
    }
    fn estimate(&self, node: usize) -> f64 {
        match (self.label, self.metric) {
            ("explicit", _) => self.graph.h[node].unwrap_or(0.0),
            (_, Some(metric)) => {
                let at = self.graph.at[node].as_deref().unwrap();
                let nearest = self
                    .goals
                    .iter()
                    .map(|&goal| metric(at, self.graph.at[goal].as_deref().unwrap()))
                    .fold(f64::INFINITY, f64::min);
                nearest * self.scale.unwrap_or(1.0)
            }
            _ => 0.0,
        }
    }
}

/// A 2D or 3D grid of cell costs; blocked cells are infinite.
struct Grid {
    size: [usize; 3],
    dims: usize,
    cost: Vec<f64>,
    moves: u8,
}
impl Grid {
    fn new(spec: &GridSpec, avoid: &[Value]) -> io::Result<Self> {
        let size = match (&spec.size, spec.rows.first()) {
            (Some(size), _) => size.clone(),
            (None, Some(row)) => vec![row.chars().count(), spec.rows.len()],
            (None, None) => return Err(io::Error::other("A grid needs size or rows.")),
        };
        if !(2..=3).contains(&size.len()) || size.contains(&0) {
            return Err(io::Error::other(
                "Grid size is [width, height] or [width, height, depth], each at least 1.",
            ));
        }
        let cells = size
            .iter()
            .try_fold(1usize, |cells, side| cells.checked_mul(*side))
            .filter(|cells| *cells <= MAX_CELLS)
            .ok_or_else(|| io::Error::other(format!("Grids are limited to {MAX_CELLS} cells.")))?;
        let dims = size.len();
        let moves = spec.moves.unwrap_or(if dims == 2 { 4 } else { 6 });
        if !matches!((dims, moves), (2, 4 | 8) | (3, 6 | 26)) {
            return Err(io::Error::other(
                "moves is 4 or 8 for 2D grids and 6 or 26 for 3D grids.",
            ));
        }
        let mut grid = Grid {
            size: [size[0], size[1], size.get(2).copied().unwrap_or(1)],
            dims,
            cost: vec![1.0; cells],
            moves,
        };
        if !spec.costs.is_empty() {
            if spec.costs.len() != cells || spec.costs.iter().any(|cost| !cost.is_finite() || *cost < 0.0) {
                return Err(io::Error::other(format!(
                    "costs needs {cells} non-negative numbers, x fastest."
                )));
            }
            grid.cost.copy_from_slice(&spec.costs);
        }
        if !spec.rows.is_empty() {
            if dims != 2
                || spec.rows.len() != grid.size[1]
                || spec.rows.iter().any(|row| row.chars().count() != grid.size[0])
            {
                return Err(io::Error::other(
                    "rows describe a 2D grid: one string per y, each width characters long.",
                ));
            }
            for (y, row) in spec.rows.iter().enumerate() {
                for (x, c) in row.chars().enumerate() {
                    grid.cost[y * grid.size[0] + x] *= match c {
                        '#' => f64::INFINITY,
                        '1'..='9' => f64::from(c as u8 - b'0'),
                        _ => 1.0,
                    };
                }
            }
        }
        let closed = spec
            .blocked
            .iter()
            .map(|at| grid.locate(at))
            .chain(avoid.iter().map(|at| grid.cell(at)))
            .collect::<io::Result<Vec<_>>>()?;
        for cell in closed {
            grid.cost[cell] = f64::INFINITY;
        }
        Ok(grid)
    }
    fn locate(&self, at: &[i64]) -> io::Result<usize> {
        let inside = at.len() == self.dims && at.iter().zip(self.size).all(|(x, side)| (0..side as i64).contains(x));
        if !inside {
            return Err(io::Error::other(format!(
                "Cell {at:?} is outside the {}D grid {:?}.",
                self.dims,
                &self.size[..self.dims]
            )));
        }
        Ok(at[0] as usize + self.size[0] * (at[1] as usize + self.size[1] * at.get(2).copied().unwrap_or(0) as usize))
    }
    fn cell(&self, value: &Value) -> io::Result<usize> {
        let at = Vec::<i64>::deserialize(value)
            .map_err(|_| io::Error::other("Grid cells are [x, y] or [x, y, z] integer arrays."))?;
        self.locate(&at)
    }
    fn coords(&self, cell: usize) -> Vec<usize> {
        let (x, rest) = (cell % self.size[0], cell / self.size[0]);
        let mut at = vec![x, rest % self.size[1]];
        if self.dims == 3 {
            at.push(rest / self.size[1]);
        }
        at
    }
}
/// One move: its offset, length, and the orthogonal cells it passes, which
/// must be open so diagonals never cut a blocked corner.
struct Move {
    delta: [i64; 3],
    length: f64,
    corners: Vec<[i64; 3]>,
}
struct GridSpace {
    grid: Grid,
    moves: Vec<Move>,
    goals: Vec<[i64; 3]>,
    /// Cheapest open cell cost: the estimate's per-step lower bound.
    floor: f64,
}
impl GridSpace {
    fn new(grid: Grid, goals: &[usize], estimate: bool) -> Self {
        let z = if grid.dims == 3 { -1..=1 } else { 0..=0 };
        let diagonal = matches!(grid.moves, 8 | 26);
        let mut moves = Vec::new();
        for dz in z {
            for dy in -1..=1_i64 {
                for dx in -1..=1_i64 {
                    let delta = [dx, dy, dz];
                    let axes = delta.iter().filter(|d| **d != 0).count();
                    if axes == 0 || (axes > 1 && !diagonal) {
                        continue;
                    }
                    // Every proper, non-empty subset of the move's axes.
                    let mut corners = (1..1_usize << 3)
                        .map(|mask| [0, 1, 2].map(|axis| if mask & (1 << axis) != 0 { delta[axis] } else { 0 }))
                        .filter(|corner| *corner != [0; 3] && *corner != delta)
                        .collect::<Vec<_>>();
                    corners.sort_unstable();
                    corners.dedup();
                    moves.push(Move {
                        delta,
                        length: (axes as f64).sqrt(),
                        corners,
                    });
                }
            }
        }
        let floor = if estimate {
            grid.cost
                .iter()
                .copied()
                .filter(|cost| cost.is_finite())
                .fold(f64::INFINITY, f64::min)
        } else {
            0.0
        };
        let goals = goals
            .iter()
            .map(|&goal| {
                let at = grid.coords(goal);
                [at[0] as i64, at[1] as i64, at.get(2).copied().unwrap_or(0) as i64]
            })
            .collect();
        GridSpace {
            grid,
            moves,
            goals,
            floor: if floor.is_finite() { floor } else { 0.0 },
        }
    }
    fn label(&self) -> &'static str {
        match self.grid.moves {
            _ if self.floor == 0.0 => "none",
            4 | 6 => "manhattan",
            _ => "octile",
        }
    }
    fn at(&self, cell: usize) -> [i64; 3] {
        let [w, h, _] = self.grid.size;
        [(cell % w) as i64, (cell / w % h) as i64, (cell / (w * h)) as i64]
    }
    fn open(&self, at: [i64; 3]) -> Option<usize> {
        let [w, h, d] = self.grid.size;
        let inside = (0..w as i64).contains(&at[0]) && (0..h as i64).contains(&at[1]) && (0..d as i64).contains(&at[2]);
        let cell = inside.then(|| at[0] as usize + w * (at[1] as usize + h * at[2] as usize))?;
        self.grid.cost[cell].is_finite().then_some(cell)
    }
}
impl Space for GridSpace {
    fn len(&self) -> usize {
        self.grid.cost.len()
    }
    fn successors(&self, node: usize, out: &mut Vec<(usize, f64)>) {
        let at = self.at(node);
        for step in &self.moves {
            let shifted = |delta: [i64; 3]| [at[0] + delta[0], at[1] + delta[1], at[2] + delta[2]];
            let Some(next) = self.open(shifted(step.delta)) else {
                continue;
            };
            if step.corners.iter().all(|corner| self.open(shifted(*corner)).is_some()) {
                out.push((next, self.grid.cost[next] * step.length));
            }
        }
    }
    fn estimate(&self, node: usize) -> f64 {
        if self.floor == 0.0 {
            return 0.0;
        }
        let at = self.at(node);
        let diagonal = matches!(self.grid.moves, 8 | 26);
        let nearest = self
            .goals
            .iter()
            .map(|goal| {
                let mut d = [0, 1, 2].map(|axis| (at[axis] - goal[axis]).unsigned_abs() as f64);
                if !diagonal {
                    return d.iter().sum();
                }
                // Octile distance: the longest axis plus diagonal savings.
                d.sort_by(|a, b| b.total_cmp(a));
                d[0] + (2f64.sqrt() - 1.0) * d[1] + (3f64.sqrt() - 2f64.sqrt()) * d[2]
            })
            .fold(f64::INFINITY, f64::min);
        nearest * self.floor
    }
}

#[cfg(test)]
mod tests;
