//! The Bots page process tree: steering chats, the agents and bots they
//! spawned, the services those own and the processes running inside them,
//! drawn as a tidy tree in server-rendered SVG.
use crate::State;

const WIDTH: f64 = 196.0;
const HEIGHT: f64 = 52.0;
const H_GAP: f64 = 22.0;
const V_GAP: f64 = 54.0;
const TREE_GAP: f64 = 44.0;
const MAX_TREES: usize = 8;
const MAX_DEPTH: usize = 16;

/// Tidy layout of an ordered tree rooted at 0: Buchheim, Jünger and Leipert's
/// linear-time form of Walker's algorithm, the n-ary generalisation of
/// Reingold–Tilford. Parents are centred over their children, siblings keep
/// their order, identical subtrees are drawn identically and adjacent nodes on
/// a level are at least one unit apart. Returns (x in units, depth) per node.
pub(crate) fn layout(children: &[Vec<usize>]) -> Vec<(f64, usize)> {
    let n = children.len();
    if n == 0 {
        return Vec::new();
    }
    let mut walk = Walk {
        children,
        parent: vec![None; n],
        number: vec![0; n],
        prelim: vec![0.0; n],
        modifier: vec![0.0; n],
        shift: vec![0.0; n],
        change: vec![0.0; n],
        thread: vec![None; n],
        ancestor: (0..n).collect(),
    };
    for (v, kids) in children.iter().enumerate() {
        for (number, &w) in kids.iter().enumerate() {
            walk.parent[w] = Some(v);
            walk.number[w] = number;
        }
    }
    walk.first_walk(0);
    let mut placed = vec![(0.0, 0); n];
    walk.second_walk(0, 0.0, 0, &mut placed);
    let left = placed.iter().map(|(x, _)| *x).fold(f64::INFINITY, f64::min);
    placed.iter_mut().for_each(|(x, _)| *x -= left);
    placed
}

struct Walk<'a> {
    children: &'a [Vec<usize>],
    parent: Vec<Option<usize>>,
    number: Vec<usize>,
    prelim: Vec<f64>,
    modifier: Vec<f64>,
    shift: Vec<f64>,
    change: Vec<f64>,
    thread: Vec<Option<usize>>,
    ancestor: Vec<usize>,
}
impl Walk<'_> {
    fn left_sibling(&self, v: usize) -> Option<usize> {
        let parent = self.parent[v]?;
        (self.number[v] > 0).then(|| self.children[parent][self.number[v] - 1])
    }
    fn leftmost_sibling(&self, v: usize) -> usize {
        self.parent[v].map_or(v, |parent| self.children[parent][0])
    }
    fn next_left(&self, v: usize) -> Option<usize> {
        self.children[v].first().copied().or(self.thread[v])
    }
    fn next_right(&self, v: usize) -> Option<usize> {
        self.children[v].last().copied().or(self.thread[v])
    }
    fn first_walk(&mut self, v: usize) {
        let children = self.children;
        let kids = &children[v];
        if kids.is_empty() {
            self.prelim[v] = self.left_sibling(v).map_or(0.0, |w| self.prelim[w] + 1.0);
            return;
        }
        let mut default_ancestor = kids[0];
        for &w in kids {
            self.first_walk(w);
            default_ancestor = self.apportion(w, default_ancestor);
        }
        self.execute_shifts(v);
        let midpoint = (self.prelim[kids[0]] + self.prelim[kids[kids.len() - 1]]) / 2.0;
        match self.left_sibling(v) {
            Some(w) => {
                self.prelim[v] = self.prelim[w] + 1.0;
                self.modifier[v] = self.prelim[v] - midpoint;
            }
            None => self.prelim[v] = midpoint,
        }
    }
    /// Pushes `v`'s subtree right until its left contour clears the right
    /// contour of its left siblings, spreading the shift over the subtrees in
    /// between so small siblings stay evenly spaced.
    fn apportion(&mut self, v: usize, mut default_ancestor: usize) -> usize {
        let Some(w) = self.left_sibling(v) else { return default_ancestor };
        let (mut vip, mut vop, mut vim, mut vom) = (v, v, w, self.leftmost_sibling(v));
        let (mut sip, mut sop, mut sim, mut som) = (self.modifier[vip], self.modifier[vop], self.modifier[vim], self.modifier[vom]);
        while let (Some(right), Some(left)) = (self.next_right(vim), self.next_left(vip)) {
            let (Some(outer_left), Some(outer_right)) = (self.next_left(vom), self.next_right(vop)) else { break };
            (vim, vip, vom, vop) = (right, left, outer_left, outer_right);
            self.ancestor[vop] = v;
            let shift = (self.prelim[vim] + sim) - (self.prelim[vip] + sip) + 1.0;
            if shift > 0.0 {
                let ancestor = self.ancestor_of(vim, v, default_ancestor);
                self.move_subtree(ancestor, v, shift);
                sip += shift;
                sop += shift;
            }
            sim += self.modifier[vim];
            sip += self.modifier[vip];
            som += self.modifier[vom];
            sop += self.modifier[vop];
        }
        if self.next_right(vim).is_some() && self.next_right(vop).is_none() {
            self.thread[vop] = self.next_right(vim);
            self.modifier[vop] += sim - sop;
        }
        if self.next_left(vip).is_some() && self.next_left(vom).is_none() {
            self.thread[vom] = self.next_left(vip);
            self.modifier[vom] += sip - som;
            default_ancestor = v;
        }
        default_ancestor
    }
    fn ancestor_of(&self, vim: usize, v: usize, default_ancestor: usize) -> usize {
        let ancestor = self.ancestor[vim];
        if self.parent[ancestor] == self.parent[v] { ancestor } else { default_ancestor }
    }
    fn move_subtree(&mut self, wm: usize, wp: usize, shift: f64) {
        let subtrees = (self.number[wp] - self.number[wm]) as f64;
        self.change[wp] -= shift / subtrees;
        self.shift[wp] += shift;
        self.change[wm] += shift / subtrees;
        self.prelim[wp] += shift;
        self.modifier[wp] += shift;
    }
    fn execute_shifts(&mut self, v: usize) {
        let (mut shift, mut change) = (0.0, 0.0);
        for &w in self.children[v].iter().rev() {
            self.prelim[w] += shift;
            self.modifier[w] += shift;
            change += self.change[w];
            shift += self.shift[w] + change;
        }
    }
    fn second_walk(&self, v: usize, m: f64, depth: usize, placed: &mut [(f64, usize)]) {
        placed[v] = (self.prelim[v] + m, depth);
        for &w in &self.children[v] {
            self.second_walk(w, m + self.modifier[v], depth + 1, placed);
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Steering,
    Bot,
    Agent,
    Service,
    Process,
}
struct Node {
    kind: Kind,
    title: String,
    detail: String,
    href: Option<String>,
    active: bool,
}
struct Tree {
    nodes: Vec<Node>,
    children: Vec<Vec<usize>>,
}
impl Tree {
    fn add(&mut self, parent: Option<usize>, node: Node) -> usize {
        self.nodes.push(node);
        self.children.push(Vec::new());
        let index = self.nodes.len() - 1;
        if let Some(parent) = parent {
            self.children[parent].push(index);
        }
        index
    }
}

fn latest_status<'a>(state: &'a State, chat: &str) -> Option<&'a str> {
    state.runtime.runs.iter().rev().find(|run| run.chat_id.as_deref() == Some(chat)).map(|run| run.status.as_str())
}
fn busy(status: Option<&str>) -> bool {
    matches!(status, Some("queued" | "running" | "waiting" | "cancelling"))
}

/// Task trees with something to show: a bot, a live service or an active turn
/// anywhere below the steering chat. Most recent first.
fn forest(state: &State) -> Vec<Tree> {
    let mut kids: std::collections::HashMap<&str, Vec<&str>> = std::collections::HashMap::new();
    for chat in &state.chats {
        if let Some(parent) = chat.parent_chat.as_deref().filter(|parent| state.chats.iter().any(|c| c.id == *parent)) {
            kids.entry(parent).or_default().push(chat.id.as_str());
        }
    }
    let relevant = |chat: &str| {
        crate::bots::defined(state, chat) || busy(latest_status(state, chat))
            || state.services.iter().any(|s| s.live() && s.resource.owner.chat.as_str() == chat)
    };
    let roots = state.chats.iter().rev()
        .filter(|chat| chat.parent_chat.as_deref().is_none_or(|parent| !state.chats.iter().any(|c| c.id == parent)))
        .map(|chat| chat.id.as_str());
    let mut trees = Vec::new();
    for root in roots {
        let mut subtree = vec![root];
        let mut index = 0;
        while index < subtree.len() {
            if let Some(children) = kids.get(subtree[index]) {
                subtree.extend(children.iter().copied());
            }
            index += 1;
        }
        if !subtree.iter().any(|chat| relevant(chat)) {
            continue;
        }
        let mut tree = Tree { nodes: Vec::new(), children: Vec::new() };
        add_chat(state, &kids, &mut tree, None, root, None, 0);
        trees.push(tree);
        if trees.len() == MAX_TREES {
            break;
        }
    }
    trees
}
/// Sibling titles often share a spawning template; drop the shared leading
/// words so each node shows what distinguishes it.
fn distinct_titles(titles: &[&str]) -> Vec<String> {
    let words: Vec<Vec<&str>> = titles.iter().map(|title| title.split_whitespace().collect()).collect();
    let common = |a: &[&str], b: &[&str]| a.iter().zip(b).take_while(|(x, y)| x == y).count();
    words.iter().enumerate().zip(titles).map(|((i, mine), title)| {
        let shared = words.iter().enumerate().filter(|(j, _)| *j != i).map(|(_, other)| common(mine, other)).max().unwrap_or(0);
        if shared >= 2 && mine.len() > shared { mine[shared..].join(" ") } else { (*title).to_owned() }
    }).collect()
}
fn add_chat(state: &State, kids: &std::collections::HashMap<&str, Vec<&str>>, tree: &mut Tree, parent: Option<usize>, chat: &str, title: Option<String>, depth: usize) {
    let session = state.chats.iter().find(|c| c.id == chat);
    let status = latest_status(state, chat);
    let bot = crate::bots::status(state, chat);
    let kind = if parent.is_none() { Kind::Steering } else if bot.is_some() { Kind::Bot } else { Kind::Agent };
    let detail = bot.unwrap_or_else(|| status.map_or("no turns".into(), |status| format!("turn {status}")));
    let node = tree.add(parent, Node {
        kind,
        title: title.unwrap_or_else(|| session.map_or(chat, |s| s.title.as_str()).to_owned()),
        detail,
        href: crate::chat::valid_id(chat).then(|| format!("/chat/{chat}")),
        active: busy(status),
    });
    if depth >= MAX_DEPTH {
        return;
    }
    let children = kids.get(chat).cloned().unwrap_or_default();
    let titles: Vec<&str> = children.iter().map(|child| state.chats.iter().find(|c| c.id == *child).map_or(*child, |c| c.title.as_str())).collect();
    for (child, title) in children.iter().zip(distinct_titles(&titles)) {
        add_chat(state, kids, tree, Some(node), child, Some(title), depth + 1);
    }
    for service in state.services.iter().filter(|s| s.live() && s.resource.owner.chat.as_str() == chat) {
        let id = service.resource.id.as_str();
        let running = matches!(service.resource.phase, control_core::resources::Phase::Running(_));
        let shape = service.shape_name();
        let service_node = tree.add(Some(node), Node {
            kind: Kind::Service,
            title: format!("{shape} service {}", id.trim_start_matches("svc-").chars().take(6).collect::<String>()),
            detail: format!("{} · {}", service.phase_name(), service.label()),
            href: Some("/tools/services".into()),
            active: running,
        });
        for watch in state.watches.iter().filter(|w| w.service == id && w.active()) {
            tree.add(Some(service_node), Node {
                kind: Kind::Process,
                title: watch.name.clone(),
                detail: if watch.permanent { "watch · restarts".into() } else { "watch".into() },
                href: None,
                active: true,
            });
        }
        for command in service.running_commands() {
            tree.add(Some(service_node), Node { kind: Kind::Process, title: command, detail: "command".into(), href: None, active: true });
        }
    }
}

fn clip(text: &str, limit: usize) -> String {
    let text = text.lines().next().unwrap_or_default();
    if text.chars().count() <= limit {
        text.to_owned()
    } else {
        format!("{}…", text.chars().take(limit - 1).collect::<String>())
    }
}

/// The SVG drawing, or None when no task tree has a bot, service or active turn.
pub(crate) fn render(state: &State) -> Option<String> {
    let trees = forest(state);
    if trees.is_empty() {
        return None;
    }
    let (mut body, mut offset, mut height) = (String::new(), 0.0_f64, 0.0_f64);
    for tree in &trees {
        let placed = layout(&tree.children);
        let span = placed.iter().map(|(x, _)| *x).fold(0.0, f64::max);
        let depth = placed.iter().map(|(_, depth)| *depth).max().unwrap_or(0);
        let at = |index: usize| {
            let (x, depth) = placed[index];
            (offset + x * (WIDTH + H_GAP), depth as f64 * (HEIGHT + V_GAP))
        };
        for (parent, children) in tree.children.iter().enumerate() {
            let (px, py) = at(parent);
            for &child in children {
                let (cx, cy) = at(child);
                let (x1, y1, x2, y2) = (px + WIDTH / 2.0, py + HEIGHT, cx + WIDTH / 2.0, cy);
                let middle = (y1 + y2) / 2.0;
                body.push_str(&format!("<path class='pt-edge' d='M{x1:.1} {y1:.1}C{x1:.1} {middle:.1} {x2:.1} {middle:.1} {x2:.1} {y2:.1}'/>"));
            }
        }
        for (index, node) in tree.nodes.iter().enumerate() {
            let (x, y) = at(index);
            let class = match node.kind {
                Kind::Steering => "pt-steering",
                Kind::Bot => "pt-bot",
                Kind::Agent => "pt-agent",
                Kind::Service => "pt-service",
                Kind::Process => "pt-process",
            };
            let full = format!("{} — {}", node.title, node.detail);
            let mut group = format!(
                "<g class='pt-node {class}' transform='translate({x:.1} {y:.1})'><title>{}</title><rect width='{WIDTH}' height='{HEIGHT}' rx='3'/><text class='pt-title' x='12' y='21'>{}</text><text class='pt-detail' x='12' y='39'>{}</text>{}</g>",
                crate::escape(&full),
                crate::escape(&clip(&node.title, 24)),
                crate::escape(&clip(&node.detail, 30)),
                if node.active { format!("<circle class='pt-live' cx='{}' cy='12' r='3'/>", WIDTH - 12.0) } else { String::new() },
            );
            if let Some(href) = &node.href {
                group = format!("<a href='{}'>{group}</a>", crate::escape(href));
            }
            body.push_str(&group);
        }
        offset += (span + 1.0) * (WIDTH + H_GAP) - H_GAP + TREE_GAP;
        height = height.max((depth + 1) as f64 * (HEIGHT + V_GAP) - V_GAP);
    }
    let width = (offset - TREE_GAP).max(WIDTH);
    Some(format!(
        "<div class='process-tree' role='region' aria-label='Process tree' tabindex='0'><svg role='img' aria-label='Task trees: steering chats, agents, bots, services and processes' viewBox='-2 -2 {:.0} {:.0}' width='{:.0}' height='{:.0}'>{body}</svg></div><p class='muted tiny'>Purple: steering chats and bots · grey: agents · cyan: services · dim: watches and running commands · dot: active.</p>",
        width + 4.0, height + 4.0, width + 4.0, height + 4.0
    ))
}

#[cfg(test)]
mod tests;
