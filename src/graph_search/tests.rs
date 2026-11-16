use super::*;

fn run(args: Value) -> Value {
    call(&args).unwrap_or_else(|error| panic!("{error}: {args}"))
}
fn error(args: Value) -> String {
    call(&args).unwrap_err().to_string()
}
/// Deterministic xorshift, so random cases reproduce.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
}
/// A random geometric graph: `n` points in a square, each joined to `k`
/// random others with cost = length × factor in [stretch, stretch + 1).
fn geometric(n: usize, k: usize, stretch: f64, seed: u64) -> (Vec<Value>, Vec<Value>) {
    let mut rng = Rng(seed);
    let points = (0..n).map(|_| [rng.unit() * 1000.0, rng.unit() * 1000.0]).collect::<Vec<_>>();
    let nodes = points.iter().enumerate().map(|(id, at)| json!({"id":id,"at":at})).collect();
    let mut edges = Vec::new();
    for from in 0..n {
        for _ in 0..k {
            let to = rng.below(n as u64) as usize;
            let length = euclidean(&points[from], &points[to]);
            edges.push(json!([from, to, length * (stretch + rng.unit())]));
        }
    }
    (nodes, edges)
}

#[test]
fn dijkstra_finds_the_cheapest_path_and_respects_direction() {
    let edges = json!([["a","b",1],["b","c",1],["a","c",5],["c","d"],{"from":"d","to":"e","cost":0.5}]);
    let found = run(json!({"edges":edges,"start":"a","goal":"e"}));
    assert_eq!((&found["path"], &found["cost"], &found["algorithm"]), (&json!(["a","b","c","d","e"]), &json!(3.5), &json!("dijkstra")));
    assert_eq!(found["steps"], 4);
    let back = run(json!({"edges":edges,"start":"e","goal":"a"}));
    assert_eq!(back["cost"], 3.5, "undirected by default");
    let directed = run(json!({"edges":edges,"directed":true,"start":"e","goal":"a"}));
    assert_eq!((&directed["found"], &directed["reason"]), (&json!(false), &json!("unreachable")));
    let integers = run(json!({"edges":[[1,2,2],[2,3,2]],"start":1,"goal":3}));
    assert_eq!(integers["path"], json!([1, 2, 3]));
    assert_eq!(run(json!({"edges":edges,"start":"a","goal":"a"}))["steps"], 0);
}

#[test]
fn astar_matches_dijkstra_costs_and_expands_less() {
    let (mut astar, mut dijkstra) = (0, 0);
    for seed in 1..=40 {
        // Stretch 1 keeps every edge at least its length; 0.3 forces scaling.
        let stretch = if seed % 2 == 0 { 1.0 } else { 0.3 };
        let (nodes, edges) = geometric(300, 4, stretch, seed);
        let goal = (seed * 7 % 300) as usize;
        let guided = run(json!({"nodes":nodes,"edges":edges,"start":0,"goal":goal}));
        let plain = run(json!({"nodes":nodes,"edges":edges,"start":0,"goal":goal,"heuristic":"none"}));
        assert_eq!(guided["found"], plain["found"], "seed {seed}");
        if plain["found"] == true {
            let (a, d) = (guided["cost"].as_f64().unwrap(), plain["cost"].as_f64().unwrap());
            assert!((a - d).abs() < 1e-6, "seed {seed}: A* {a} vs Dijkstra {d}");
            assert_eq!(guided["heuristic"], "euclidean");
            if stretch < 1.0 {
                assert!(guided["heuristic_scale"].as_f64().unwrap() < 1.0, "an edge shorter than its length scales the estimate");
            } else {
                assert!(guided.get("heuristic_scale").is_none());
            }
            astar += guided["expanded"].as_u64().unwrap();
            dijkstra += plain["expanded"].as_u64().unwrap();
        }
    }
    assert!(astar * 2 < dijkstra, "A* {astar} vs Dijkstra {dijkstra} expansions");
}

#[test]
fn manhattan_and_explicit_heuristics_stay_optimal() {
    let nodes = json!([{"id":"a","at":[0,0]},{"id":"b","at":[1,0]},{"id":"c","at":[1,1]},{"id":"d","at":[0,1]}]);
    let edges = json!([["a","b",1],["b","c",1],["a","d",1],["d","c",1.5]]);
    let found = run(json!({"nodes":nodes,"edges":edges,"start":"a","goal":"c","heuristic":"manhattan"}));
    assert_eq!((&found["path"], &found["cost"], &found["heuristic"]), (&json!(["a","b","c"]), &json!(2.0), &json!("manhattan")));
    // Explicit estimates: consistent ones are reported as such.
    let estimates = json!([{"id":"a","h":2},{"id":"b","h":1},{"id":"c","h":0},{"id":"d","h":1}]);
    let explicit = run(json!({"nodes":estimates,"edges":edges,"start":"a","goal":"c"}));
    assert_eq!((&explicit["heuristic"], &explicit["consistent"], &explicit["cost"]), (&json!("explicit"), &json!(true), &json!(2.0)));
    // Admissible but inconsistent: re-expansion still returns the optimum.
    let tricky = json!([{"id":"s","h":0},{"id":"a","h":4},{"id":"b","h":0},{"id":"c","h":0},{"id":"g","h":0}]);
    let tricky_edges = json!([["s","a",1],["s","b",1],["a","c",1],["b","c",4],["c","g",4]]);
    let found = run(json!({"nodes":tricky,"edges":tricky_edges,"directed":true,"start":"s","goal":"g"}));
    assert_eq!((&found["cost"], &found["consistent"]), (&json!(6.0), &json!(false)));
    assert_eq!(found["path"], json!(["s","a","c","g"]));
    assert!(error(json!({"nodes":["a"],"edges":[["a","b"]],"start":"a","goal":"b","heuristic":"euclidean"})).contains("at on every node"));
}

#[test]
fn goals_constraints_and_cost_limits() {
    let edges = json!([["s","a",1],["a","g1",5],["s","b",2],["b","g2",2],["s","g2",9]]);
    let nearest = run(json!({"edges":edges,"start":"s","goals":["g1","g2"]}));
    assert_eq!((&nearest["goal"], &nearest["cost"]), (&json!("g2"), &json!(4.0)));
    let avoided = run(json!({"edges":edges,"start":"s","goals":["g1","g2"],"avoid":["b"]}));
    assert_eq!((&avoided["goal"], &avoided["cost"]), (&json!("g1"), &json!(6.0)));
    let cut = run(json!({"edges":edges,"start":"s","goal":"g2","avoid_edges":[["g2","b"]]}));
    assert_eq!(cut["cost"], 9.0, "an undirected avoided edge is cut both ways");
    let capped = run(json!({"edges":edges,"start":"s","goal":"g2","max_cost":3.5}));
    assert_eq!((&capped["found"], &capped["reason"]), (&json!(false), &json!("no path within max_cost")));
    assert_eq!(run(json!({"edges":edges,"start":"s","goal":"g2","max_cost":4}))["cost"], 4.0, "the limit is inclusive");
}

#[test]
fn grids_route_around_walls_without_cutting_corners() {
    let rows = json!(["S..#....", ".#.#.##.", ".#...#..", ".####.#.", "......#G"]);
    let four = run(json!({"grid":{"rows":rows},"start":[0,0],"goal":[7,4]}));
    let plain = run(json!({"grid":{"rows":rows},"start":[0,0],"goal":[7,4],"heuristic":"none"}));
    assert_eq!((&four["heuristic"], &plain["algorithm"]), (&json!("manhattan"), &json!("dijkstra")));
    assert_eq!(four["cost"], plain["cost"]);
    assert_eq!(four["cost"], 15.0);
    assert_eq!(four["path"][0], json!([0, 0]));
    assert_eq!(four["path"][15], json!([7, 4]));
    // Diagonals are shorter but may not squeeze between two walls.
    let corner = json!({"grid":{"rows":[".#","#."],"moves":8},"start":[0,0],"goal":[1,1]});
    assert_eq!(run(corner)["found"], false);
    let eight = run(json!({"grid":{"rows":["...","...","..."],"moves":8},"start":[0,0],"goal":[2,2]}));
    assert!((eight["cost"].as_f64().unwrap() - 2.0 * 2f64.sqrt()).abs() < 1e-9);
    assert_eq!(eight["heuristic"], "octile");
    // Digits are cell costs; every route crosses the 9s once.
    let weighted = run(json!({"grid":{"rows":["...","999","..."]},"start":[0,0],"goal":[0,2]}));
    assert_eq!(weighted["cost"], 10.0);
    let flat = run(json!({"grid":{"size":[3,3],"costs":[1,1,1,9,9,1,1,1,1]},"start":[0,0],"goal":[0,2]}));
    assert_eq!(flat["path"], json!([[0,0],[1,0],[2,0],[2,1],[2,2],[1,2],[0,2]]));
    let blocked = run(json!({"grid":{"size":[3,3],"blocked":[[1,0],[1,1]]},"avoid":[[1,2]],"start":[0,0],"goal":[2,0]}));
    assert_eq!(blocked["found"], false);
}

#[test]
fn three_dimensional_grids_and_random_mazes_agree_with_dijkstra() {
    let six = run(json!({"grid":{"size":[4,4,4]},"start":[0,0,0],"goal":[3,3,3]}));
    assert_eq!(six["cost"], 9.0);
    let diagonal = run(json!({"grid":{"size":[4,4,4],"moves":26},"start":[0,0,0],"goal":[3,3,3]}));
    assert!((diagonal["cost"].as_f64().unwrap() - 3.0 * 3f64.sqrt()).abs() < 1e-9);
    let mut rng = Rng(99);
    for (size, moves) in [(json!([40, 40]), 4), (json!([40, 40]), 8), (json!([12, 12, 12]), 6), (json!([12, 12, 12]), 26)] {
        let cells = size.as_array().unwrap().iter().map(|side| side.as_u64().unwrap()).product::<u64>();
        let costs = (0..cells).map(|_| 1 + rng.below(4)).collect::<Vec<_>>();
        let dims = size.as_array().unwrap().len();
        let mut blocked = Vec::new();
        for _ in 0..cells / 4 {
            let at = (0..dims).map(|axis| rng.below(size[axis].as_u64().unwrap())).collect::<Vec<_>>();
            if at.iter().any(|x| *x != 0) && at.iter().zip(size.as_array().unwrap()).any(|(x, side)| *x != side.as_u64().unwrap() - 1) {
                blocked.push(at);
            }
        }
        let goal = size.as_array().unwrap().iter().map(|side| side.as_u64().unwrap() - 1).collect::<Vec<_>>();
        let start = vec![0; dims];
        let grid = json!({"size":size,"costs":costs,"blocked":blocked,"moves":moves});
        let guided = run(json!({"grid":grid,"start":start,"goal":goal}));
        let plain = run(json!({"grid":grid,"start":start,"goal":goal,"heuristic":"none"}));
        assert_eq!(guided["found"], plain["found"]);
        if plain["found"] == true {
            assert!((guided["cost"].as_f64().unwrap() - plain["cost"].as_f64().unwrap()).abs() < 1e-6, "{moves}-connected");
            assert!(guided["expanded"].as_u64() <= plain["expanded"].as_u64());
        }
    }
}

#[test]
fn inputs_are_bounded_and_errors_are_clear() {
    let many = (0..=MAX_NODES).map(|id| json!(id)).collect::<Vec<_>>();
    assert!(error(json!({"nodes":many,"start":0,"goal":1})).contains("20000 nodes"));
    let edges = (0..=MAX_EDGES).map(|id| json!([id % 100, (id + 1) % 100])).collect::<Vec<_>>();
    assert!(error(json!({"edges":edges,"start":0,"goal":1})).contains("100000 edges"));
    assert!(error(json!({"grid":{"size":[1001,1000]},"start":[0,0],"goal":[1,1]})).contains("1000000 cells"));
    assert!(error(json!({"grid":{"size":[10,10]},"start":[0,0],"goal":[10,0]})).contains("outside"));
    assert!(error(json!({"grid":{"size":[10,10],"moves":6},"start":[0,0],"goal":[1,0]})).contains("moves"));
    assert!(error(json!({"edges":[["a","b"]],"start":"a","goal":"z"})).contains("Unknown node z"));
    assert!(error(json!({"edges":[["a","b",-1]],"start":"a","goal":"b"})).contains("non-negative"));
    assert!(error(json!({"edges":[["a","b"]],"start":"a"})).contains("goal"));
    assert!(error(json!({"edges":[["a","b"]],"grid":{"size":[2,2]},"start":"a","goal":"b"})).contains("not both"));
    assert!(error(json!({"edges":[["a","b"]],"start":"a","goal":"b","colour":"red"})).contains("Invalid search input"));
    let capped = error(json!({"grid":{"size":[100,100]},"start":[0,0],"goal":[99,99],"heuristic":"none","max_expansions":50}));
    assert!(capped.contains("after 50 expansions"), "{capped}");
}

/// Large-input timings, printed for docs/PERFORMANCE.md:
/// `cargo test --release --locked --bin control graph_search::tests::profile -- --ignored --nocapture`
#[test]
#[ignore]
fn profile() {
    let time = |label: &str, args: &Value| {
        let mut samples = (0..5).map(|_| {
            let began = Instant::now();
            let found = call(args).unwrap();
            (began.elapsed(), found)
        }).collect::<Vec<_>>();
        samples.sort_by_key(|(elapsed, _)| *elapsed);
        let (elapsed, found) = &samples[2];
        println!("{label}: median {:.1} ms, expanded {}, cost {:.1}", elapsed.as_secs_f64() * 1e3, found["expanded"], found["cost"].as_f64().unwrap_or(f64::NAN));
    };
    let (nodes, edges) = geometric(MAX_NODES, MAX_EDGES / MAX_NODES, 1.0, 7);
    let graph = json!({"nodes":nodes,"edges":edges,"start":0,"goal":MAX_NODES - 1});
    let began = Instant::now();
    let input = Input::deserialize(&graph).unwrap();
    println!("parse 20000 nodes / 100000 edges: {:.1} ms", began.elapsed().as_secs_f64() * 1e3);
    drop(input);
    time("graph 20k/100k, A* euclidean", &graph);
    let mut plain = graph.clone();
    plain["heuristic"] = json!("none");
    time("graph 20k/100k, Dijkstra", &plain);
    let mut rng = Rng(5);
    let blocked = (0..100_000).map(|_| [rng.below(1000), rng.below(1000)]).filter(|at| at != &[0, 0] && at != &[999, 999]).collect::<Vec<_>>();
    for moves in [4, 8] {
        let grid = json!({"grid":{"size":[1000,1000],"blocked":blocked,"moves":moves},"start":[0,0],"goal":[999,999]});
        time(&format!("grid 1000x1000 10% blocked, {moves}-connected, A*"), &grid);
        let mut plain = grid.clone();
        plain["heuristic"] = json!("none");
        time(&format!("grid 1000x1000 10% blocked, {moves}-connected, Dijkstra"), &plain);
    }
    let cube = json!({"grid":{"size":[100,100,100],"moves":26},"start":[0,0,0],"goal":[99,99,99]});
    time("grid 100^3 open, 26-connected, A*", &cube);
}
