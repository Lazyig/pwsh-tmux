//! The mode tree (tmux mode-tree.c) and the modes built on it: tree mode (choose-tree,
//! window-tree.c), buffer mode (choose-buffer), client mode (choose-client) and customize
//! mode (in customize.rs).
//!
//! The items are rebuilt from the server whenever the mode is drawn or gets a key, so the
//! list follows changes; what the user changed (expanded and tagged items, the current
//! item and the scroll position) is kept by item tag, as tmux keeps it across rebuilds.

use super::customize;
use super::prompt::{PanePrompt, PromptKey};
use super::{expand_as, format_cells, mode_style, preview_into, put_cells, FmtType, ModeAction};
use crate::cells::{Cell, Color, Line, Style};
use crate::server::input::fill_template;
use crate::server::vars::Target;
use crate::server::*;
use std::collections::HashSet;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TreeKind {
    /// choose-tree: sessions, windows and panes.
    Tree,
    Buffers,
    Clients,
    /// customize-mode: options, hooks, environment and key bindings.
    Options,
}

/// What choose-tree starts with collapsed: -s sessions, -w windows.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Collapse {
    Sessions,
    Windows,
    None,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Preview {
    Off,
    Normal,
    Big,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Sort {
    Index,
    Name,
    Activity,
    Z,
    Creation,
    Size,
    Modifier,
    Order,
}

impl Sort {
    fn name(self) -> &'static str {
        match self {
            Sort::Index => "index",
            Sort::Name => "name",
            Sort::Activity => "activity",
            Sort::Z => "z",
            Sort::Creation => "creation",
            Sort::Size => "size",
            Sort::Modifier => "modifier",
            Sort::Order => "order",
        }
    }

    /// sort_order_from_string.
    pub fn parse(s: &str) -> Option<Sort> {
        Some(match s.to_ascii_lowercase().as_str() {
            "index" | "key" => Sort::Index,
            "name" | "title" => Sort::Name,
            "activity" => Sort::Activity,
            "z" => Sort::Z,
            "creation" => Sort::Creation,
            "size" => Sort::Size,
            "modifier" => Sort::Modifier,
            "order" => Sort::Order,
            _ => return None,
        })
    }
}

/// sort_qsort: "order" keeps the natural order (reversed with -r), anything else sorts with
/// `cmp`, whose ties the caller breaks by name.
fn sort_by<T>(v: &mut [T], order: Sort, reversed: bool, cmp: impl Fn(&T, &T) -> std::cmp::Ordering) {
    if v.len() < 2 {
        return;
    }
    if order == Sort::Order {
        if reversed {
            v.reverse();
        }
        return;
    }
    v.sort_by(|a, b| {
        let r = cmp(a, b);
        if reversed { r.reverse() } else { r }
    });
}

#[derive(Clone, Debug, PartialEq)]
pub enum ItemData {
    Session(SessId),
    Window(SessId, u32),
    Pane(SessId, u32, PaneId),
    Buffer(String),
    Client(ClientId),
    Custom(customize::Item),
}

pub struct Node {
    pub tag: String,
    pub name: String,
    pub text: Option<String>,
    pub data: ItemData,
    pub parent: Option<usize>,
    pub children: Vec<usize>,
    pub expanded: bool,
    pub tagged: bool,
    /// The name is right-aligned with the others at its depth.
    pub align: bool,
    pub no_tag: bool,
    /// The preview is of the parent (customize key details).
    pub draw_as_parent: bool,
}

pub struct TLine {
    pub item: usize,
    pub depth: usize,
    pub last: bool,
    pub flat: bool,
    pub key: Option<String>,
}

pub struct Built {
    pub nodes: Vec<Node>,
    pub roots: Vec<usize>,
    pub lines: Vec<TLine>,
    pub no_matches: bool,
}

pub struct Builder<'a> {
    tree: &'a Tree,
    pub nodes: Vec<Node>,
    pub roots: Vec<usize>,
}

impl Builder<'_> {
    /// mode_tree_add: `expanded` is 0 or 1, or -1 for expanded (and not saved as collapsed).
    pub fn add(&mut self, parent: Option<usize>, tag: String, name: String, text: Option<String>, data: ItemData, expanded: i8) -> usize {
        let parent_expanded = parent.is_none_or(|p| self.nodes[p].expanded);
        let expanded = match self.tree.expanded.get(&tag) {
            Some(e) => *e,
            None => expanded != 0,
        };
        let tagged = parent_expanded && self.tree.tagged.contains(&tag);
        let i = self.nodes.len();
        self.nodes.push(Node { tag, name, text, data, parent, children: vec![], expanded, tagged, align: false, no_tag: false, draw_as_parent: false });
        match parent {
            Some(p) => self.nodes[p].children.push(i),
            None => self.roots.push(i),
        }
        i
    }

    /// mode_tree_remove: the node stays in the arena but is no longer in the tree.
    pub fn remove(&mut self, i: usize) {
        match self.nodes[i].parent {
            Some(p) => self.nodes[p].children.retain(|c| *c != i),
            None => self.roots.retain(|c| *c != i),
        }
    }
}

pub enum TreePrompt {
    Search,
    Filter,
    /// `:` in tree mode: a command for each tagged item.
    Command,
    KillCurrent,
    KillTagged,
    Customize(customize::PromptFor),
}

const PREFIX_STYLE: &str = "#[fg=themelightgrey]#[bg=default]";
const BORDER_X: &str = "#[#{E:tree-mode-border-style}]│#[default]";

const HELP_START: &[(&str, &str)] = &[
    ("      Up, k", "Move cursor up"),
    ("    Down, j", "Move cursor down"),
    ("          g", "Go to top"),
    ("          G", "Go to bottom"),
    (" PPage, C-b", "Page up"),
    (" NPage, C-f", "Page down"),
    ("    Left, h", "Collapse %1"),
    ("   Right, l", "Expand %1"),
    ("        M--", "Collapse all %1s"),
    ("        M-+", "Expand all %1s"),
    ("          t", "Toggle %1 tag"),
    ("          T", "Untag all %1s"),
    ("        C-t", "Tag all %1s"),
    ("        C-s", "Search forward"),
    ("          n", "Repeat search forward"),
    ("          N", "Repeat search backward"),
    ("          f", "Filter %1s"),
    ("          O", "Change sort order"),
    ("          r", "Reverse sort order"),
    ("          v", "Toggle preview"),
];
const HELP_END: &[(&str, &str)] = &[("  q, Escape", "Exit mode")];
const HELP_TREE: &[(&str, &str)] = &[
    ("      Enter", "Choose selected item"),
    ("       S-Up", "Swap current and previous window"),
    ("     S-Down", "Swap current and next window"),
    ("          x", "Kill selected item"),
    ("          X", "Kill tagged items"),
    ("          <", "Scroll previews left"),
    ("          >", "Scroll previews right"),
    ("          m", "Set the marked pane"),
    ("          M", "Clear the marked pane"),
    ("          i", "Toggle session, window and pane information"),
    ("          :", "Run a command for each tagged item"),
    ("          f", "Enter a format"),
    ("          H", "Jump to the starting pane"),
];
const HELP_BUFFER: &[(&str, &str)] = &[
    ("      Enter", "Paste selected %1"),
    ("          p", "Paste selected %1"),
    ("          P", "Paste tagged %1s"),
    ("          d", "Delete selected %1"),
    ("          D", "Delete tagged %1s"),
    ("          e", "Open %1 in editor"),
    ("          f", "Enter a filter"),
];
const HELP_CLIENT: &[(&str, &str)] = &[
    ("          i", "Toggle info view"),
    ("      Enter", "Choose selected %1"),
    ("          d", "Detach selected %1"),
    ("          D", "Detach tagged %1s"),
    ("          x", "Detach selected %1"),
    ("          X", "Detach tagged %1s"),
    ("          z", "Suspend selected %1"),
    ("          Z", "Suspend tagged %1s"),
    ("          f", "Enter a filter"),
];

pub const TREE_DEFAULT_COMMAND: &str = "switch-client -Zt '%%'";
pub const BUFFER_DEFAULT_COMMAND: &str = "paste-buffer -p -b '%%'";
pub const CLIENT_DEFAULT_COMMAND: &str = "detach-client -t '%%'";

pub const TREE_DEFAULT_FORMAT: &str = "#{?pane_format,#{?pane_marked,#[fg=thememagenta],}#{?pane_floating_flag,#[underscore],}#{pane_current_command}#[fg=themelightgrey]#{pane_flags}#{?#{&&:#{pane_title},#{!=:#{pane_title},#{host_short}}},: \"#{pane_title}\",},window_format,#{?window_marked_flag,#[fg=thememagenta],}#{window_name}#[fg=themelightgrey]#{window_flags}#{?#{&&:#{==:#{window_panes},1},#{&&:#{pane_title},#{!=:#{pane_title},#{host_short}}}},: \"#{pane_title}\",},#[fg=themelightgrey]#{session_windows} windows#{?session_grouped, (group #{session_group}: #{session_group_list}),}#{?session_attached, (attached),}}";
pub const BUFFER_DEFAULT_FORMAT: &str = "#{t/p:buffer_created}: #{buffer_sample}";
pub const CLIENT_DEFAULT_FORMAT: &str = "#[fg=themelightgrey]#{t/p:client_activity}: session #[default]#{session_name}";

fn flag(label: &str, cond: &str) -> String {
    format!("#{{?{cond},#[fg=themegreen],#[fg=themelightgrey]}}{label}#[default]")
}

fn info_line(label: &str, value: &str) -> String {
    format!("#[fg=themelightgrey]{label:<14}{BORDER_X} {value}")
}

pub struct Tree {
    pub kind: TreeKind,
    pub owner: PaneId,
    pub command: String,
    pub format: String,
    pub key_format: Option<String>,
    pub expanded: HashMap<String, bool>,
    pub tagged: HashSet<String>,
    pub cur_tag: Option<String>,
    pub cur: usize,
    pub offset: usize,
    pub preview: Preview,
    pub filter: Option<String>,
    pub search: Option<String>,
    pub help: bool,
    pub prompt: Option<(TreePrompt, PanePrompt)>,
    pub order: Option<Sort>,
    pub reversed: bool,
    /// choose-tree -s/-w until the first build; later items are expanded (WINDOW_TREE_NONE).
    pub collapse: Collapse,
    built_once: bool,
    /// -h: hide the pane containing the mode.
    pub hide_this_pane: bool,
    /// Without -G only the current (or first) session of each group is shown.
    pub squash_groups: bool,
    /// i: information instead of the preview.
    pub info: bool,
    /// < and >: scroll the list of previews.
    pub preview_offset: std::cell::Cell<i32>,
    /// -y: no confirmation prompts.
    pub accept: bool,
    /// The target the mode was opened for (H, and the first selection).
    pub origin: Target,
    /// customize-mode C and H.
    pub hide_default: bool,
    pub hide_global: bool,
    /// -Z: Some(was already zoomed).
    pub zoomed: Option<bool>,
    /// -k: kill the pane when the mode exits.
    pub kill: bool,
    pub client: Option<ClientId>,
    /// The last list drawn: mode_tree_draw leaves the screen alone when the list is empty
    /// (customize C with nothing changed), so the old items stay on screen.
    last_grid: std::cell::RefCell<Option<Vec<Vec<Cell>>>>,
    /// Prompt answers for the prompt history, taken by the server after each key.
    history: Vec<(usize, String)>,
}

impl Tree {
    pub fn new(kind: TreeKind, owner: PaneId, origin: Target) -> Tree {
        let (command, format) = match kind {
            TreeKind::Tree => (TREE_DEFAULT_COMMAND, TREE_DEFAULT_FORMAT),
            TreeKind::Buffers => (BUFFER_DEFAULT_COMMAND, BUFFER_DEFAULT_FORMAT),
            TreeKind::Clients => (CLIENT_DEFAULT_COMMAND, CLIENT_DEFAULT_FORMAT),
            TreeKind::Options => ("", customize::DEFAULT_FORMAT),
        };
        Tree {
            kind,
            owner,
            command: command.to_string(),
            format: format.to_string(),
            key_format: None,
            expanded: HashMap::new(),
            tagged: HashSet::new(),
            cur_tag: None,
            cur: 0,
            offset: 0,
            preview: Preview::Normal,
            filter: None,
            search: None,
            help: false,
            prompt: None,
            order: None,
            reversed: false,
            collapse: Collapse::None,
            built_once: false,
            hide_this_pane: false,
            squash_groups: true,
            info: false,
            preview_offset: std::cell::Cell::new(0),
            accept: false,
            origin,
            hide_default: false,
            hide_global: false,
            zoomed: None,
            kill: false,
            client: None,
            last_grid: std::cell::RefCell::new(None),
            history: Vec::new(),
        }
    }

    /// The sort orders O cycles through (the mode's order_seq); none for customize mode.
    fn order_seq(&self) -> &'static [Sort] {
        match self.kind {
            TreeKind::Tree => &[Sort::Index, Sort::Name, Sort::Activity, Sort::Z],
            TreeKind::Buffers => &[Sort::Creation, Sort::Name, Sort::Size],
            TreeKind::Clients => &[Sort::Name, Sort::Size, Sort::Creation, Sort::Activity],
            TreeKind::Options => &[],
        }
    }

    /// The order in use: -O as given (any order, as mode_tree_start keeps it), else the
    /// first of the mode's O cycle (window_*_sort).
    pub fn sort(&self) -> Option<Sort> {
        let seq = self.order_seq();
        seq.first()?;
        Some(self.order.unwrap_or(seq[0]))
    }

    /// Called once the mode is set up: records what the first build expanded and selects
    /// the item the mode was opened for (window_tree_build's tag for data->type).
    pub fn start(&mut self, srv: &Server) {
        let b = self.build(srv);
        for n in &b.nodes {
            self.expanded.insert(n.tag.clone(), n.expanded);
        }
        if self.kind == TreeKind::Tree {
            let o = self.origin;
            let tag = match (self.collapse, o.sid, o.idx, o.pane) {
                (Collapse::Sessions, Some(s), _, _) => Some(format!("s{s}")),
                (Collapse::Windows, Some(s), Some(i), _) => Some(format!("w{s}:{i}")),
                (Collapse::None, Some(s), Some(i), Some(p)) => {
                    let single = o.wid.and_then(|w| srv.windows.get(&w)).is_some_and(|w| w.panes().len() == 1);
                    Some(if single { format!("w{s}:{i}") } else { format!("p{p}") })
                }
                _ => None,
            };
            if let Some(i) = tag.and_then(|t| b.lines.iter().position(|l| b.nodes[l.item].tag == t)) {
                self.cur = i;
            }
        }
        self.built_once = true;
        let b = self.build(srv);
        self.store_current(&b);
    }

    // ---- building --------------------------------------------------------------------

    pub fn build(&self, srv: &Server) -> Built {
        let filter = self.filter.as_deref().filter(|f| !f.is_empty());
        let (mut nodes, mut roots) = self.build_with(srv, filter);
        let no_matches = roots.is_empty();
        if no_matches && filter.is_some() {
            (nodes, roots) = self.build_with(srv, None);
        }
        let mut lines = Vec::new();
        Tree::build_lines(&nodes, &roots, 0, &mut lines);
        for (i, l) in lines.iter_mut().enumerate() {
            l.key = self.line_key(srv, &nodes[l.item], i);
        }
        Built { nodes, roots, lines, no_matches: no_matches && filter.is_some() }
    }

    fn build_with(&self, srv: &Server, filter: Option<&str>) -> (Vec<Node>, Vec<usize>) {
        let mut b = Builder { tree: self, nodes: vec![], roots: vec![] };
        match self.kind {
            TreeKind::Tree => self.build_tree(srv, &mut b, filter),
            TreeKind::Buffers => self.build_buffers(srv, &mut b, filter),
            TreeKind::Clients => self.build_clients(srv, &mut b, filter),
            TreeKind::Options => customize::build(self, srv, &mut b, filter),
        }
        (b.nodes, b.roots)
    }

    fn build_lines(nodes: &[Node], list: &[usize], depth: usize, out: &mut Vec<TLine>) {
        let flat = list.iter().all(|i| nodes[*i].children.is_empty());
        for (n, i) in list.iter().enumerate() {
            out.push(TLine { item: *i, depth, last: n + 1 == list.len(), flat, key: None });
            if nodes[*i].expanded {
                Tree::build_lines(nodes, &nodes[*i].children, depth + 1, out);
            }
        }
    }

    /// The shortcut key of a line: -K expanded for the item, else 0-9 then M-a to M-z.
    fn line_key(&self, srv: &Server, node: &Node, line: usize) -> Option<String> {
        let Some(kf) = &self.key_format else {
            return match line {
                0..=9 => Some(line.to_string()),
                10..=35 => Some(format!("M-{}", (b'a' + (line - 10) as u8) as char)),
                _ => None,
            };
        };
        let (t, ty) = self.item_target(srv, &node.data);
        let mut extra = HashMap::new();
        extra.insert("line".to_string(), line.to_string());
        if let ItemData::Buffer(name) = &node.data {
            if let Some(b) = srv.buffers.iter().find(|b| &b.name == name) {
                extra.extend(crate::server::cmds::buffer_vars(b));
            }
        }
        let s = expand_as(srv, t, ty, kf, extra);
        crate::keys::parse_key_name(&s).map(|k| crate::keys::key_name(&k))
    }

    /// The format target of an item and its type.
    fn item_target(&self, srv: &Server, d: &ItemData) -> (Target, FmtType) {
        match d {
            ItemData::Session(s) => {
                let Some(sess) = srv.sessions.get(s) else { return (Target::default(), FmtType::None) };
                let wid = sess.active_window();
                (Target { sid: Some(*s), idx: Some(sess.active), wid: Some(wid), pane: srv.windows.get(&wid).map(|w| w.active) }, FmtType::Session)
            }
            ItemData::Window(s, i) => {
                let wid = srv.sessions.get(s).and_then(|s| s.links.get(i)).copied();
                (Target { sid: Some(*s), idx: Some(*i), wid, pane: wid.and_then(|w| srv.windows.get(&w)).map(|w| w.active) }, FmtType::Window)
            }
            ItemData::Pane(s, i, p) => {
                let wid = srv.sessions.get(s).and_then(|s| s.links.get(i)).copied();
                (Target { sid: Some(*s), idx: Some(*i), wid, pane: Some(*p) }, FmtType::Pane)
            }
            _ => (srv.target_for_pane(self.owner), FmtType::None),
        }
    }

    fn filter_true(srv: &Server, filter: Option<&str>, t: Target, ty: FmtType, extra: HashMap<String, String>) -> bool {
        match filter {
            None => true,
            Some(f) => crate::server::cmds::truthy(&expand_as(srv, t, ty, f, extra)),
        }
    }

    /// sort_get_sessions: by name, then sort_session_cmp.
    fn sorted_sessions<'s>(&self, srv: &'s Server) -> Vec<&'s Session> {
        let mut v: Vec<&Session> = srv.sessions.values().collect();
        v.sort_by(|a, b| a.name.cmp(&b.name));
        let order = self.sort().unwrap_or(Sort::Index);
        sort_by(&mut v, order, self.reversed, |a, b| {
            match order {
                Sort::Index => a.id.cmp(&b.id),
                Sort::Creation => (a.created, a.id).cmp(&(b.created, b.id)),
                Sort::Activity => b.activity_seq.cmp(&a.activity_seq),
                Sort::Name => a.name.cmp(&b.name),
                _ => std::cmp::Ordering::Equal,
            }
            .then(a.name.cmp(&b.name))
        });
        v
    }

    fn build_tree(&self, srv: &Server, b: &mut Builder, filter: Option<&str>) {
        let order = self.sort().unwrap_or(Sort::Index);
        let after = self.built_once;
        let current_group = self.origin.sid.and_then(|s| srv.sessions.get(&s)).and_then(|s| s.group);
        for s in self.sorted_sessions(srv) {
            if self.squash_groups {
                if let Some(g) = s.group {
                    let first = srv.sessions.values().filter(|x| x.group == Some(g)).map(|x| x.id).min();
                    if (Some(g) == current_group && Some(s.id) != self.origin.sid) || (Some(g) != current_group && Some(s.id) != first) {
                        continue;
                    }
                }
            }
            let (t, _) = self.item_target(srv, &ItemData::Session(s.id));
            let text = expand_as(srv, t, FmtType::Session, &self.format, HashMap::new());
            let expanded = if after || self.collapse != Collapse::Sessions { 1 } else { 0 };
            let si = b.add(None, format!("s{}", s.id), s.name.clone(), Some(text), ItemData::Session(s.id), expanded);
            // sort_get_winlinks_session: by index, then sort_winlink_cmp
            let mut wls: Vec<(u32, WinId)> = s.links.iter().map(|(i, w)| (*i, *w)).collect();
            sort_by(&mut wls, order, self.reversed, |a, c| {
                let (Some(wa), Some(wc)) = (srv.windows.get(&a.1), srv.windows.get(&c.1)) else { return a.0.cmp(&c.0) };
                match order {
                    Sort::Index => a.0.cmp(&c.0),
                    Sort::Creation => (wa.created, wa.id).cmp(&(wc.created, wc.id)),
                    Sort::Activity => wc.activity_us.cmp(&wa.activity_us),
                    Sort::Name => wa.name.cmp(&wc.name),
                    Sort::Size => (wa.sx as u32 * wa.sy as u32).cmp(&(wc.sx as u32 * wc.sy as u32)),
                    _ => std::cmp::Ordering::Equal,
                }
                .then(wa.name.cmp(&wc.name))
            });
            let mut empty = 0;
            for (idx, wid) in &wls {
                if !self.build_window(srv, b, s, *idx, *wid, si, filter, after) {
                    empty += 1;
                }
            }
            if empty == wls.len() {
                b.remove(si);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn build_window(&self, srv: &Server, b: &mut Builder, s: &Session, idx: u32, wid: WinId, parent: usize, filter: Option<&str>, after: bool) -> bool {
        let Some(w) = srv.windows.get(&wid) else { return false };
        let t = Target { sid: Some(s.id), idx: Some(idx), wid: Some(wid), pane: Some(w.active) };
        let text = expand_as(srv, t, FmtType::Window, &self.format, HashMap::new());
        let expanded = if after || self.collapse == Collapse::None { 1 } else { 0 };
        let wi = b.add(Some(parent), format!("w{}:{idx}", s.id), idx.to_string(), Some(text), ItemData::Window(s.id, idx), expanded);
        b.nodes[wi].align = true;
        let base = srv.opt_window_num(Some(wid), "pane-base-index").max(0) as usize;
        let mut panes: Vec<(usize, PaneId)> = w.panes().into_iter().enumerate().map(|(i, p)| (i + base, p)).collect();
        // sort_pane_cmp: ties go by title and -r reverses the comparison; activity is the
        // active_point (focus order: never active, then the last active, then the active)
        let order = self.sort().unwrap_or(Sort::Index);
        let title = |p: PaneId| srv.panes.get(&p).map(|p| p.title.clone()).unwrap_or_default();
        let point = |p: PaneId| if p == w.active { 2 } else if w.last() == Some(p) { 1 } else { 0 };
        let size = |p: PaneId| srv.panes.get(&p).map_or(0, |p| p.size.0 as u32 * p.size.1 as u32);
        sort_by(&mut panes, order, self.reversed, |a, c| {
            match order {
                Sort::Activity => point(a.1).cmp(&point(c.1)),
                Sort::Creation => a.1.cmp(&c.1),
                Sort::Size => size(a.1).cmp(&size(c.1)),
                Sort::Index | Sort::Z => a.0.cmp(&c.0),
                Sort::Name => title(a.1).cmp(&title(c.1)),
                _ => std::cmp::Ordering::Equal,
            }
            .then_with(|| title(a.1).cmp(&title(c.1)))
        });
        let mut found = 0;
        for (pi, p) in panes {
            let pt = Target { pane: Some(p), ..t };
            if !Tree::filter_true(srv, filter, pt, FmtType::Pane, HashMap::new()) {
                continue;
            }
            found += 1;
            if self.hide_this_pane && p == self.owner {
                continue;
            }
            let text = expand_as(srv, pt, FmtType::Pane, &self.format, HashMap::new());
            let ni = b.add(Some(wi), format!("p{p}"), pi.to_string(), Some(text), ItemData::Pane(s.id, idx, p), -1);
            b.nodes[ni].align = true;
        }
        if found == 0 {
            b.remove(wi);
            return false;
        }
        true
    }

    fn build_buffers(&self, srv: &Server, b: &mut Builder, filter: Option<&str>) {
        // sort_get_buffers: newest first (paste_walk), then sort_buffer_cmp
        let mut bufs: Vec<&Buffer> = srv.buffers.iter().collect();
        let order = self.sort().unwrap_or(Sort::Creation);
        let pos = |b: &Buffer| srv.buffers.iter().position(|x| x.name == b.name).unwrap_or(0);
        sort_by(&mut bufs, order, self.reversed, |a, c| {
            match order {
                Sort::Name => a.name.cmp(&c.name),
                Sort::Creation => pos(a).cmp(&pos(c)),
                Sort::Size => a.data.len().cmp(&c.data.len()),
                _ => std::cmp::Ordering::Equal,
            }
            .then(a.name.cmp(&c.name))
        });
        let t = srv.target_for_pane(self.owner);
        for buf in bufs {
            let vars = crate::server::cmds::buffer_vars(buf);
            if !Tree::filter_true(srv, filter, t, FmtType::None, vars.clone()) {
                continue;
            }
            let text = expand_as(srv, t, FmtType::None, &self.format, vars);
            b.add(None, format!("b{}", buf.name), buf.name.clone(), Some(text), ItemData::Buffer(buf.name.clone()), -1);
        }
    }

    fn client_target(srv: &Server, c: &Client) -> Target {
        match c.session.and_then(|s| srv.sessions.get(&s)) {
            Some(s) => {
                let wid = s.active_window();
                Target { sid: Some(s.id), idx: Some(s.active), wid: Some(wid), pane: srv.windows.get(&wid).map(|w| w.active) }
            }
            None => Target::default(),
        }
    }

    fn build_clients(&self, srv: &Server, b: &mut Builder, filter: Option<&str>) {
        // sort_get_clients: in the order they connected, then sort_client_cmp
        let mut cs: Vec<&Client> = srv.clients.values().filter(|c| c.attached && c.session.is_some()).collect();
        cs.sort_by_key(|c| c.id);
        let order = self.sort().unwrap_or(Sort::Name);
        sort_by(&mut cs, order, self.reversed, |a, c| {
            match order {
                Sort::Name => a.name().cmp(&c.name()),
                Sort::Size => a.size.0.cmp(&c.size.0).then(a.size.1.cmp(&c.size.1)),
                Sort::Creation => a.created.cmp(&c.created),
                Sort::Activity => c.activity.cmp(&a.activity),
                _ => std::cmp::Ordering::Equal,
            }
            .then(a.name().cmp(&c.name()))
        });
        for c in cs {
            let t = Tree::client_target(srv, c);
            let ctx = crate::server::vars::VarCtx { srv, t, client: Some(c.id), extra: HashMap::new() };
            if let Some(f) = filter {
                if !crate::server::cmds::truthy(&crate::format::expand(f, &ctx)) {
                    continue;
                }
            }
            let text = crate::format::expand(&self.format, &ctx);
            b.add(None, format!("c{}", c.id), c.name(), Some(text), ItemData::Client(c.id), -1);
        }
    }

    // ---- state -----------------------------------------------------------------------

    /// mode_tree_set_height: the height of the list.
    pub fn list_height(&self, sy: usize, lines: usize) -> usize {
        let mut height = if self.preview == Preview::Off {
            sy
        } else if self.kind == TreeKind::Options {
            // window_customize_height
            if 12 < sy { sy - 12 } else { sy }
        } else if self.preview == Preview::Normal {
            let mut h = (sy / 3) * 2;
            if h > lines {
                h = sy / 2;
            }
            if h < 10 { sy } else { h }
        } else {
            (sy / 4).min(lines).max(2)
        };
        if sy.saturating_sub(height) < 2 {
            height = sy;
        }
        height
    }

    /// The current line and the scroll offset for these lines (mode_tree_set_current and
    /// mode_tree_check_selected): the offset is kept unless the current line would be off
    /// screen or the list shrank.
    fn resolve(&self, b: &Built, height: usize) -> (usize, usize) {
        let n = b.lines.len();
        if n == 0 {
            return (0, 0);
        }
        let cur = self.cur_tag.as_ref().and_then(|t| b.lines.iter().position(|l| &b.nodes[l.item].tag == t)).unwrap_or(self.cur.min(n - 1));
        let mut offset = self.offset;
        if height == 0 {
            return (cur, offset);
        }
        if n <= height {
            offset = 0;
        } else if offset > n - height {
            offset = n - height;
        }
        if cur < offset {
            offset = cur;
        } else if cur > offset + height - 1 {
            offset = cur + 1 - height;
        }
        (cur, offset)
    }

    fn store_current(&mut self, b: &Built) {
        self.cur_tag = b.lines.get(self.cur).map(|l| b.nodes[l.item].tag.clone());
    }

    fn set_expanded(&mut self, b: &mut Built, item: usize, on: bool) {
        b.nodes[item].expanded = on;
        self.expanded.insert(b.nodes[item].tag.clone(), on);
    }

    fn up(&mut self, n: usize, height: usize, wrap: bool) {
        if n == 0 {
            return;
        }
        if self.cur == 0 {
            if wrap {
                self.cur = n - 1;
                if n >= height {
                    self.offset = n - height;
                }
            }
        } else {
            self.cur -= 1;
            if self.cur < self.offset {
                self.offset -= 1;
            }
        }
    }

    fn down(&mut self, n: usize, height: usize, wrap: bool) -> bool {
        if n == 0 {
            return false;
        }
        if self.cur == n - 1 {
            if !wrap {
                return false;
            }
            self.cur = 0;
            self.offset = 0;
        } else {
            self.cur += 1;
            if self.cur + 1 > self.offset + height {
                self.offset += 1;
            }
        }
        true
    }

    fn tagged_items(&self, b: &Built) -> Vec<usize> {
        b.lines.iter().map(|l| l.item).filter(|i| b.nodes[*i].tagged).collect()
    }

    /// The items for an action on tagged items, or the current item when none is tagged.
    fn tagged_or_current(&self, b: &Built) -> Vec<usize> {
        let t = self.tagged_items(b);
        if t.is_empty() { b.lines.get(self.cur).map(|l| vec![l.item]).unwrap_or_default() } else { t }
    }

    /// The target an item is chosen as (window_tree_get_target).
    fn target_name(srv: &Server, d: &ItemData) -> Option<String> {
        match d {
            ItemData::Session(s) => srv.sessions.get(s).map(|s| format!("={}:", s.name)),
            ItemData::Window(s, i) => srv.sessions.get(s).filter(|s| s.links.contains_key(i)).map(|s| format!("={}:{i}.", s.name)),
            ItemData::Pane(s, i, p) => srv.sessions.get(s).filter(|s| s.links.contains_key(i)).map(|s| format!("={}:{i}.%{p}", s.name)),
            ItemData::Buffer(name) => Some(name.clone()),
            ItemData::Client(c) => srv.clients.get(c).map(|c| c.name()),
            _ => None,
        }
    }

    fn search_matches(&self, srv: &Server, node: &Node, s: &str, icase: bool) -> bool {
        let has = |hay: &str| if icase { hay.to_lowercase().contains(&s.to_lowercase()) } else { hay.contains(s) };
        match &node.data {
            ItemData::Session(id) => srv.sessions.get(id).is_some_and(|x| has(&x.name)),
            ItemData::Window(id, i) => srv.sessions.get(id).and_then(|x| x.links.get(i)).and_then(|w| srv.windows.get(w)).is_some_and(|w| has(&w.name)),
            ItemData::Pane(_, _, p) => srv.panes.get(p).is_some_and(|p| !p.current_cmd.is_empty() && has(&p.current_cmd)),
            ItemData::Buffer(name) => has(name) || srv.buffers.iter().find(|b| &b.name == name).is_some_and(|b| has(&String::from_utf8_lossy(&b.data))),
            ItemData::Client(c) => srv.clients.get(c).is_some_and(|c| has(&c.name())),
            _ => has(&node.name),
        }
    }

    /// mode_tree_search_set: the next (or previous) item in the whole tree matching the search,
    /// expanding its parents.
    fn search_next(&mut self, srv: &Server, b: &mut Built, forward: bool) {
        let Some(s) = self.search.clone() else { return };
        let icase = s.chars().all(|c| !c.is_uppercase());
        let mut order = Vec::new();
        fn walk(nodes: &[Node], list: &[usize], out: &mut Vec<usize>) {
            for i in list {
                out.push(*i);
                walk(nodes, &nodes[*i].children, out);
            }
        }
        walk(&b.nodes, &b.roots, &mut order);
        let Some(cur) = b.lines.get(self.cur).map(|l| l.item) else { return };
        let Some(start) = order.iter().position(|i| *i == cur) else { return };
        let n = order.len();
        for k in 1..n {
            let at = if forward { (start + k) % n } else { (start + n - k) % n };
            let i = order[at];
            if !self.search_matches(srv, &b.nodes[i], &s, icase) {
                continue;
            }
            let mut p = b.nodes[i].parent;
            while let Some(pi) = p {
                self.set_expanded(b, pi, true);
                p = b.nodes[pi].parent;
            }
            self.cur_tag = Some(b.nodes[i].tag.clone());
            return;
        }
    }

    /// mode_tree_set_prompt: PROMPT_NOFORMAT and PROMPT_ISMODE, search or command history.
    pub fn set_prompt(&mut self, kind: TreePrompt, label: &str, input: &str, single: bool) {
        let mut p = PanePrompt::new(label, input);
        p.single = single;
        p.noformat = true;
        p.ismode = true;
        p.ptype = if matches!(kind, TreePrompt::Search | TreePrompt::Filter) { super::prompt::TYPE_SEARCH } else { super::prompt::TYPE_COMMAND };
        self.prompt = Some((kind, p));
    }

    /// The prompt answers to add to the prompt history (prompt_add_history), as (type, line).
    pub fn take_history(&mut self) -> Vec<(usize, String)> {
        std::mem::take(&mut self.history)
    }

    // ---- keys ------------------------------------------------------------------------

    /// A key for the mode (mode_tree_key and the mode's own keys); `w` x `h` is the pane size.
    pub fn key(&mut self, name: &str, srv: &Server, _w: usize, h: usize) -> ModeAction {
        let mut b = self.build(srv);
        let n = b.lines.len();
        if n == 0 {
            return ModeAction::Exit;
        }
        let height = self.list_height(h, n);
        (self.cur, self.offset) = self.resolve(&b, height);
        self.cur_tag = None;
        let action = self.key_inner(name, srv, &mut b, height);
        // the current item may have moved: remember it by tag for the next build
        if self.cur_tag.is_none() {
            self.store_current(&b);
        }
        action
    }

    fn key_inner(&mut self, name: &str, srv: &Server, b: &mut Built, height: usize) -> ModeAction {
        let n = b.lines.len();
        // prompt_key takes every key while the prompt is open (mode_tree_key)
        if let Some((_, prompt)) = &mut self.prompt {
            let ty = prompt.ptype.min(1);
            match prompt.key(name, &srv.prompt_history[ty]) {
                PromptKey::Handled | PromptKey::Changed | PromptKey::Move => return ModeAction::None,
                PromptKey::Cancel => {
                    self.prompt = None;
                    return ModeAction::None;
                }
                PromptKey::Done(s) | PromptKey::Numeric(s) => {
                    let (kind, p) = self.prompt.take().unwrap();
                    // prompt_add_history for what was typed (not a single-key answer)
                    if !s.is_empty() && !p.single {
                        self.history.push((ty, s.clone()));
                    }
                    return self.prompt_done(kind, s, srv, b);
                }
            }
        }
        if self.help {
            if !matches!(name, "FocusIn" | "FocusOut") {
                self.help = false;
            }
            return ModeAction::None;
        }
        let mut name = name.to_string();
        if let Some(i) = b.lines.iter().position(|l| l.key.as_deref() == Some(name.as_str())) {
            self.cur = i;
            name = "Enter".into();
        }
        let name = name.as_str();
        let item = b.lines[self.cur].item;
        let line_flat = b.lines[self.cur].flat;
        match name {
            "q" | "Escape" | "C-[" | "C-g" => return ModeAction::Exit,
            "F1" | "C-h" => self.help = true,
            "Up" | "k" | "WheelUpPane" | "C-p" => self.up(n, height, true),
            "Down" | "j" | "WheelDownPane" | "C-n" => {
                self.down(n, height, true);
            }
            "S-Up" | "K" => return self.swap(srv, b, -1),
            "S-Down" | "J" => return self.swap(srv, b, 1),
            "PPage" | "C-b" => {
                for _ in 0..height {
                    if self.cur == 0 {
                        break;
                    }
                    self.up(n, height, true);
                }
            }
            "NPage" | "C-f" => {
                for _ in 0..height {
                    if self.cur == n - 1 {
                        break;
                    }
                    self.down(n, height, true);
                }
            }
            "g" | "Home" => {
                self.cur = 0;
                self.offset = 0;
            }
            "G" | "End" => {
                self.cur = n - 1;
                self.offset = if self.cur > height.saturating_sub(1) { self.cur + 1 - height } else { 0 };
            }
            "t" => {
                if !b.nodes[item].no_tag {
                    if !b.nodes[item].tagged {
                        // parents and children are never both tagged
                        let mut p = b.nodes[item].parent;
                        while let Some(pi) = p {
                            self.tagged.remove(&b.nodes[pi].tag);
                            p = b.nodes[pi].parent;
                        }
                        let mut stack = b.nodes[item].children.clone();
                        while let Some(c) = stack.pop() {
                            self.tagged.remove(&b.nodes[c].tag);
                            stack.extend(b.nodes[c].children.iter().copied());
                        }
                        self.tagged.insert(b.nodes[item].tag.clone());
                    } else {
                        self.tagged.remove(&b.nodes[item].tag);
                    }
                    // mode_tree_key: then on to the next line
                    self.down(n, height, false);
                }
            }
            "T" => {
                for l in &b.lines {
                    self.tagged.remove(&b.nodes[l.item].tag);
                }
            }
            "C-t" => {
                for l in &b.lines {
                    let node = &b.nodes[l.item];
                    let on = match node.parent {
                        None => !node.no_tag,
                        Some(p) => b.nodes[p].no_tag,
                    };
                    if on {
                        self.tagged.insert(node.tag.clone());
                    } else {
                        self.tagged.remove(&node.tag);
                    }
                }
            }
            "O" => {
                // sort_next_order: an order outside the cycle starts it again
                let seq = self.order_seq();
                if let Some(cur) = self.sort() {
                    let i = seq.iter().position(|o| *o == cur).map_or(0, |i| (i + 1) % seq.len());
                    self.order = Some(seq[i]);
                }
            }
            "r" if !self.order_seq().is_empty() => self.reversed = !self.reversed,
            "Left" | "h" | "-" => {
                let target = if line_flat || !b.nodes[item].expanded { b.nodes[item].parent } else { Some(item) };
                match target {
                    None => self.up(n, height, false),
                    Some(t) => {
                        self.set_expanded(b, t, false);
                        self.cur_tag = Some(b.nodes[t].tag.clone());
                    }
                }
            }
            "Right" | "l" | "+" => {
                if line_flat || b.nodes[item].expanded {
                    self.down(n, height, false);
                } else {
                    self.set_expanded(b, item, true);
                }
            }
            "M--" => {
                for r in b.roots.clone() {
                    self.set_expanded(b, r, false);
                }
            }
            "M-+" => {
                for r in b.roots.clone() {
                    self.set_expanded(b, r, true);
                }
            }
            "?" | "/" | "C-s" => self.set_prompt(TreePrompt::Search, "(search) ", "", false),
            "n" | "N" => self.search_next(srv, b, name == "n"),
            "f" => {
                let current = self.filter.clone().unwrap_or_default();
                self.set_prompt(TreePrompt::Filter, "(filter) ", &current, false)
            }
            "c" => {
                self.prompt = None;
                self.filter = None;
            }
            "v" => {
                self.preview = match self.preview {
                    Preview::Off => Preview::Big,
                    Preview::Normal => Preview::Off,
                    Preview::Big => Preview::Normal,
                };
            }
            _ => {}
        }
        match self.kind {
            TreeKind::Tree => self.tree_key(name, srv, b),
            TreeKind::Buffers => self.buffer_key(name, srv, b),
            TreeKind::Clients => self.client_key(name, srv, b, height),
            TreeKind::Options => customize::key(self, name, srv, b),
        }
    }

    fn current_item(&self, b: &Built) -> usize {
        b.lines[self.cur.min(b.lines.len() - 1)].item
    }

    fn swap(&mut self, srv: &Server, b: &Built, dir: i32) -> ModeAction {
        if self.kind != TreeKind::Tree {
            return ModeAction::None;
        }
        let depth = b.lines[self.cur].depth;
        let mut with = self.cur as i64;
        loop {
            with += dir as i64;
            if with < 0 || with as usize >= b.lines.len() {
                return ModeAction::None;
            }
            if b.lines[with as usize].depth <= depth {
                break;
            }
        }
        let with = with as usize;
        if b.lines[with].depth != depth {
            return ModeAction::None;
        }
        let (ItemData::Window(s1, i1), ItemData::Window(s2, i2)) = (&b.nodes[b.lines[self.cur].item].data, &b.nodes[b.lines[with].item].data) else { return ModeAction::None };
        if s1 != s2 || !srv.sessions.contains_key(s1) {
            return ModeAction::None;
        }
        // sort_would_window_tree_swap: only when the order would not put them back
        if self.sort() != Some(Sort::Index) {
            let w = |i: &u32| srv.sessions.get(s1).and_then(|s| s.links.get(i)).and_then(|w| srv.windows.get(w));
            let (Some(a), Some(c)) = (w(i1), w(i2)) else { return ModeAction::None };
            let same = match self.sort() {
                Some(Sort::Creation) => (a.created, a.id) == (c.created, c.id),
                Some(Sort::Activity) => a.activity_us == c.activity_us,
                Some(Sort::Size) => a.sx as u32 * a.sy as u32 == c.sx as u32 * c.sy as u32,
                Some(Sort::Name) | Some(Sort::Z) | Some(Sort::Modifier) | Some(Sort::Order) | None => true,
                Some(Sort::Index) => true,
            } && a.name == c.name;
            if !same {
                return ModeAction::None;
            }
        }
        self.cur_tag = Some(format!("w{s1}:{i2}"));
        // session_set_current: the session's current window follows its window to the
        // other index (and the old index becomes the last window)
        let active = srv.sessions.get(s1).map(|s| s.active);
        let select = match active {
            Some(a) if a == *i1 => format!(" ; select-window -t '${s1}:{i2}'"),
            Some(a) if a == *i2 => format!(" ; select-window -t '${s1}:{i1}'"),
            _ => String::new(),
        };
        ModeAction::RunKeep(format!("swap-window -d -s '${s1}:{i1}' -t '${s1}:{i2}'{select}"))
    }

    fn kill_command(srv: &Server, d: &ItemData) -> Option<String> {
        match d {
            ItemData::Session(s) => srv.sessions.contains_key(s).then(|| format!("kill-session -t '${s}'")),
            ItemData::Window(s, i) => srv.sessions.get(s).and_then(|x| x.links.get(i)).map(|w| format!("kill-window -t '@{w}'")),
            ItemData::Pane(_, _, p) => srv.panes.contains_key(p).then(|| format!("kill-pane -t '%{p}'")),
            _ => None,
        }
    }

    fn prompt_done(&mut self, kind: TreePrompt, s: String, srv: &Server, b: &mut Built) -> ModeAction {
        match kind {
            TreePrompt::Search => {
                self.search = Some(s).filter(|s| !s.is_empty());
                self.search_next(srv, b, true);
                ModeAction::None
            }
            TreePrompt::Filter => {
                self.filter = Some(s).filter(|s| !s.is_empty());
                ModeAction::None
            }
            TreePrompt::Command => {
                if s.is_empty() {
                    return ModeAction::None;
                }
                let cmds: Vec<String> = self.tagged_or_current(b).iter().filter_map(|i| Tree::target_name(srv, &b.nodes[*i].data)).map(|t| fill_template(&s, &[t], srv.answer_quoting())).collect();
                if cmds.is_empty() { ModeAction::None } else { ModeAction::RunKeep(cmds.join(" ; ")) }
            }
            TreePrompt::KillCurrent | TreePrompt::KillTagged => {
                if !s.eq_ignore_ascii_case("y") {
                    return ModeAction::None;
                }
                let items = if matches!(kind, TreePrompt::KillCurrent) { vec![self.current_item(b)] } else { self.tagged_or_current(b) };
                let cmds: Vec<String> = items.iter().filter_map(|i| Tree::kill_command(srv, &b.nodes[*i].data)).collect();
                if cmds.is_empty() { ModeAction::None } else { ModeAction::RunKeep(cmds.join(" ; ")) }
            }
            TreePrompt::Customize(p) => customize::prompt_done(self, p, s, srv, b),
        }
    }

    fn tree_key(&mut self, name: &str, srv: &Server, b: &mut Built) -> ModeAction {
        let item = self.current_item(b);
        let data = b.nodes[item].data.clone();
        match name {
            "<" => self.preview_offset.set(self.preview_offset.get() - 1),
            ">" => self.preview_offset.set(self.preview_offset.get() + 1),
            "H" => {
                let o = self.origin;
                if let (Some(s), Some(i)) = (o.sid, o.idx) {
                    self.expanded.insert(format!("s{s}"), true);
                    self.expanded.insert(format!("w{s}:{i}"), true);
                    let b2 = self.build(srv);
                    let want = [format!("p{}", self.owner), format!("w{s}:{i}")];
                    if let Some(t) = want.iter().find(|t| b2.lines.iter().any(|l| &&b2.nodes[l.item].tag == t)) {
                        self.cur_tag = Some(t.clone());
                    }
                }
            }
            "m" => {
                let pane = match &data {
                    ItemData::Pane(_, _, p) => Some(*p),
                    ItemData::Window(s, i) => srv.sessions.get(s).and_then(|x| x.links.get(i)).and_then(|w| srv.windows.get(w)).map(|w| w.active),
                    ItemData::Session(s) => srv.sessions.get(s).and_then(|x| srv.windows.get(&x.active_window())).map(|w| w.active),
                    _ => None,
                };
                if let Some(p) = pane {
                    return ModeAction::RunKeep(format!("select-pane -M ; select-pane -m -t '%{p}'"));
                }
            }
            "M" => return ModeAction::RunKeep("select-pane -M".into()),
            "i" => self.info = !self.info,
            "x" => {
                let label = match &data {
                    ItemData::Session(s) => srv.sessions.get(s).map(|s| format!("Kill session {}? ", s.name)),
                    ItemData::Window(s, i) => srv.sessions.get(s).filter(|x| x.links.contains_key(i)).map(|_| format!("Kill window {i}? ")),
                    ItemData::Pane(_, _, p) => srv.window_of(*p).and_then(|w| {
                        let base = srv.opt_window_num(Some(w), "pane-base-index").max(0) as usize;
                        srv.windows[&w].panes().iter().position(|x| x == p).map(|i| format!("Kill pane {}? ", i + base))
                    }),
                    _ => None,
                };
                if let Some(label) = label {
                    if self.accept {
                        return self.prompt_done(TreePrompt::KillCurrent, "y".into(), srv, b);
                    }
                    self.set_prompt(TreePrompt::KillCurrent, &label, "", true);
                }
            }
            "X" => {
                let tagged = self.tagged_items(b).len();
                if tagged != 0 {
                    if self.accept {
                        return self.prompt_done(TreePrompt::KillTagged, "y".into(), srv, b);
                    }
                    self.set_prompt(TreePrompt::KillTagged, &format!("Kill {tagged} tagged? "), "", true);
                }
            }
            ":" => {
                let tagged = self.tagged_items(b).len();
                let label = if tagged != 0 { format!("({tagged} tagged) ") } else { "(current) ".to_string() };
                self.set_prompt(TreePrompt::Command, &label, "", false);
            }
            "Enter" => {
                return match Tree::target_name(srv, &data) {
                    Some(t) => ModeAction::Run(fill_template(&self.command, &[t], srv.answer_quoting())),
                    None => ModeAction::Exit,
                };
            }
            _ => {}
        }
        ModeAction::None
    }

    fn buffer_key(&mut self, name: &str, srv: &Server, b: &mut Built) -> ModeAction {
        if srv.buffers.is_empty() {
            return ModeAction::Exit;
        }
        let item = self.current_item(b);
        let paste = |t: &Tree, items: &[usize]| -> Vec<String> {
            items.iter().filter_map(|i| Tree::target_name(srv, &b.nodes[*i].data)).map(|n| fill_template(&t.command, &[n], srv.answer_quoting())).collect()
        };
        match name {
            "d" => {
                // the next item becomes current (window_buffer_do_delete)
                self.down(b.lines.len(), usize::MAX, false);
                if let ItemData::Buffer(n) = &b.nodes[item].data {
                    return ModeAction::RunKeep(format!("delete-buffer -b {}", crate::cmdparse::quote(n)));
                }
            }
            "D" => {
                let cmds: Vec<String> = self.tagged_items(b).iter().filter_map(|i| match &b.nodes[*i].data {
                    ItemData::Buffer(n) => Some(format!("delete-buffer -b {}", crate::cmdparse::quote(n))),
                    _ => None,
                }).collect();
                if !cmds.is_empty() {
                    return ModeAction::RunKeep(cmds.join(" ; "));
                }
            }
            "P" => {
                let cmds = paste(self, &self.tagged_items(b));
                return if cmds.is_empty() { ModeAction::Exit } else { ModeAction::Run(cmds.join(" ; ")) };
            }
            "p" | "Enter" => {
                let cmds = paste(self, &[item]);
                return if cmds.is_empty() { ModeAction::Exit } else { ModeAction::Run(cmds.join(" ; ")) };
            }
            "e" => {
                if let ItemData::Buffer(n) = &b.nodes[item].data {
                    if let Some(buf) = srv.buffers.iter().find(|x| &x.name == n) {
                        return ModeAction::Edit { value: buf.data.clone(), command: String::new(), buffer: Some(n.clone()) };
                    }
                }
            }
            _ => {}
        }
        ModeAction::None
    }

    fn client_key(&mut self, name: &str, srv: &Server, b: &mut Built, height: usize) -> ModeAction {
        let item = self.current_item(b);
        let detach = |name: &str, c: &str| match name {
            "d" | "D" => format!("detach-client -t {}", crate::cmdparse::quote(c)),
            "x" | "X" => format!("detach-client -P -t {}", crate::cmdparse::quote(c)),
            _ => format!("suspend-client -t {}", crate::cmdparse::quote(c)),
        };
        match name {
            "d" | "x" | "z" => {
                let Some(c) = Tree::target_name(srv, &b.nodes[item].data) else { return ModeAction::None };
                self.down(b.lines.len(), height, false);
                return ModeAction::RunKeep(detach(name, &c));
            }
            "D" | "X" | "Z" => {
                let cmds: Vec<String> = self.tagged_items(b).iter().filter_map(|i| Tree::target_name(srv, &b.nodes[*i].data)).map(|c| detach(name, &c)).collect();
                if !cmds.is_empty() {
                    return ModeAction::RunKeep(cmds.join(" ; "));
                }
            }
            "i" => self.info = !self.info,
            "Enter" => {
                return match Tree::target_name(srv, &b.nodes[item].data) {
                    Some(c) => ModeAction::Run(fill_template(&self.command, &[c], srv.answer_quoting())),
                    None => ModeAction::Exit,
                };
            }
            _ => {}
        }
        ModeAction::None
    }

    /// Mouse keys at (x, y) in the pane (the mouse part of mode_tree_key and window_tree_mouse).
    pub fn mouse(&mut self, key: &str, x: u16, y: u16, srv: &Server, w: usize, h: usize) -> ModeAction {
        // with a prompt open, a press of the first button on its line moves its cursor and
        // other mouse keys go on to the tree (mode_tree_key)
        if let Some((_, p)) = &mut self.prompt {
            let top = srv.opt_session(srv.target_for_pane(self.owner).sid, "status-position") == "top";
            let py = if top { 0 } else { h.saturating_sub(1) };
            if key == "MouseDown1Pane" && y as usize == py {
                p.mouse(x as usize);
                return ModeAction::None;
            }
        }
        if self.help {
            return ModeAction::None;
        }
        let b = self.build(srv);
        let n = b.lines.len();
        if n == 0 {
            return ModeAction::Exit;
        }
        let height = self.list_height(h, n);
        (self.cur, self.offset) = self.resolve(&b, height);
        let (x, y) = (x as usize, y as usize);
        if x > w || y >= height {
            if key == "MouseDown3Pane" {
                return self.menu(&b, true, x, y);
            }
            // the preview: clicking a window or pane in a session or window preview chooses it
            if self.kind == TreeKind::Tree && key == "MouseDown1Pane" && self.preview != Preview::Off && !self.info {
                return self.preview_click(srv, &b, x, w);
            }
            if key.starts_with("Wheel") {
                return self.key(if key == "WheelUpPane" { "Up" } else { "Down" }, srv, w, h);
            }
            return ModeAction::None;
        }
        if self.offset + y < n {
            match key {
                "MouseDown1Pane" | "MouseDown3Pane" | "DoubleClick1Pane" => {
                    self.cur = self.offset + y;
                    self.cur_tag = Some(b.nodes[b.lines[self.cur].item].tag.clone());
                    if key == "DoubleClick1Pane" {
                        return self.key("Enter", srv, w, h);
                    }
                    if key == "MouseDown3Pane" {
                        return self.menu(&b, false, x, y);
                    }
                }
                "WheelUpPane" => return self.key("Up", srv, w, h),
                "WheelDownPane" => return self.key("Down", srv, w, h),
                _ => {}
            }
        } else if key == "MouseDown3Pane" {
            return self.menu(&b, false, x, y);
        }
        ModeAction::None
    }

    /// mode_tree_display_menu: the mode's menu for the current item, or (outside the list)
    /// the one that scrolls the previews.
    fn menu(&self, b: &Built, outside: bool, x: usize, y: usize) -> ModeAction {
        const LINE: (&str, &str, &str) = ("", "", "");
        const TAGS: [(&str, &str, &str); 3] = [("Tag", "t", "t"), ("Tag All", "[DC4]", "C-t"), ("Tag None", "T", "T")];
        const CANCEL: (&str, &str, &str) = ("Cancel", "q", "q");
        let items: Vec<(&'static str, &'static str, &'static str)> = if outside {
            vec![("Scroll Left", "<", "<"), ("Scroll Right", ">", ">"), LINE, CANCEL]
        } else {
            let mut v = match self.kind {
                TreeKind::Tree => vec![("Select", "Enter", "Enter"), ("Expand", "Right", "Right"), ("Mark", "m", "m"), LINE],
                TreeKind::Buffers => vec![("Paste", "p", "p"), ("Paste Tagged", "P", "P"), LINE],
                TreeKind::Clients => vec![("Detach", "d", "d"), ("Detach Tagged", "D", "D"), LINE],
                TreeKind::Options => vec![("Select", "Enter", "Enter"), ("Edit", "e", "e"), ("Expand", "Right", "Right"), LINE],
            };
            v.extend(TAGS);
            v.push(LINE);
            match self.kind {
                TreeKind::Tree => v.extend([("Kill", "x", "x"), ("Kill Tagged", "X", "X"), LINE]),
                TreeKind::Buffers => v.extend([("Delete", "d", "d"), ("Delete Tagged", "D", "D"), LINE]),
                TreeKind::Options => v.extend([("Changed Only", "C", "C"), LINE]),
                TreeKind::Clients => {}
            }
            v.push(CANCEL);
            v
        };
        let title = if outside { String::new() } else { b.lines.get(self.cur).map(|l| format!("#[align=centre]{}", b.nodes[l.item].name)).unwrap_or_default() };
        ModeAction::Menu { title, items, x: x as u16, y: y as u16 }
    }

    fn preview_click(&mut self, srv: &Server, b: &Built, x: usize, w: usize) -> ModeAction {
        let item = b.lines[self.cur].item;
        let data = b.nodes[item].data.clone();
        let sx = w.saturating_sub(4);
        let x = x.saturating_sub(2);
        let children: Vec<String> = match &data {
            ItemData::Session(s) => srv.sessions.get(s).map(|x| x.links.keys().map(|i| format!("w{s}:{i}")).collect()).unwrap_or_default(),
            ItemData::Window(s, i) => srv.sessions.get(s).and_then(|x| x.links.get(i)).and_then(|w| srv.windows.get(w)).map(|w| w.panes().into_iter().filter(|p| !(self.hide_this_pane && *p == self.owner)).map(|p| format!("p{p}")).collect()).unwrap_or_default(),
            _ => return ModeAction::None,
        };
        let Some(geo) = self.strip_geometry(children.len(), self.strip_current(srv, &data), sx) else { return ModeAction::None };
        if geo.left && x <= 2 {
            self.preview_offset.set(self.preview_offset.get() - 1);
            return ModeAction::None;
        }
        if geo.right && x >= sx - 3 {
            self.preview_offset.set(self.preview_offset.get() + 1);
            return ModeAction::None;
        }
        let rel = if geo.left { x.saturating_sub(3) } else { x };
        let k = (rel / geo.each.max(1)).min(geo.end - geo.start - 1);
        let Some(tag) = children.get(geo.start + k) else { return ModeAction::None };
        self.expanded.insert(b.nodes[item].tag.clone(), true);
        self.cur_tag = Some(tag.clone());
        let d = match &data {
            ItemData::Session(s) => ItemData::Window(*s, tag.split(':').nth(1).and_then(|i| i.parse().ok()).unwrap_or(0)),
            ItemData::Window(s, i) => ItemData::Pane(*s, *i, tag[1..].parse().unwrap_or(0)),
            _ => return ModeAction::None,
        };
        match Tree::target_name(srv, &d) {
            Some(t) => ModeAction::Run(fill_template(&self.command, &[t], srv.answer_quoting())),
            None => ModeAction::None,
        }
    }

    // ---- drawing ---------------------------------------------------------------------

    #[cfg(test)]
    pub fn render(&self, srv: &Server, w: u16, h: u16) -> (Vec<Line>, Option<(u16, u16)>) {
        self.render_rows(srv, w, h, 0..h as usize)
    }

    /// The mode's screen with only `rows` drawn: the other rows are empty and take no
    /// writes (a huge floating pane shows a few of its rows).
    pub fn render_rows(&self, srv: &Server, w: u16, h: u16, rows: std::ops::Range<usize>) -> (Vec<Line>, Option<(u16, u16)>) {
        let (wu, hu) = (w as usize, h as usize);
        let mut grid: Vec<Vec<Cell>> = (0..hu).map(|y| if rows.contains(&y) { vec![Cell::default(); wu] } else { Vec::new() }).collect();
        let b = self.build(srv);
        let t = srv.target_for_pane(self.owner);
        let wid = t.wid;
        let n = b.lines.len();
        let height = self.list_height(hu, n);
        let (cur, offset) = self.resolve(&b, height);
        if n == 0 {
            if let Some(last) = self.last_grid.borrow().as_ref().filter(|g| g.len() == hu && g.iter().zip(&grid).all(|(l, n)| l.len() == n.len())) {
                grid = last.clone();
            }
        } else if wu != 0 && hu != 0 {
            self.draw_list(srv, &b, &mut grid, wu, height, cur, offset, t);
            if self.preview != Preview::Off && !(hu <= 4 || height < 2 || hu - height <= 4 || wu <= 4) {
                let item = b.lines[cur].item;
                let item = if b.nodes[item].draw_as_parent { b.nodes[item].parent.unwrap_or(item) } else { item };
                let border = mode_style(srv, &srv.opt_window(wid, "tree-mode-border-style"), Style::default(), t);
                super::draw_box(&mut grid, 0, height, wu, hu - height, border);
                let mut title = format!(" {}", b.nodes[item].name);
                if let Some(o) = self.sort() {
                    title.push_str(&format!(" (sort: {}{})", o.name(), if self.reversed { ", reversed" } else { "" }));
                    if let Some(v) = self.view_name() {
                        title.push_str(&format!(" (view: {v})"));
                    }
                }
                let tw = crate::format::width(&title);
                if wu - 2 >= tw {
                    let label = if b.no_matches { "no matches" } else { "active" };
                    let mut s = title.clone();
                    if self.filter.is_some() && wu - 2 >= tw + 10 + label.len() + 2 {
                        s.push_str(&format!(" (filter: {label}) "));
                    } else {
                        s.push(' ');
                    }
                    super::put_text(&mut grid, 1, height, &s, border, wu);
                }
                let (bx, by) = (wu - 4, hu - height - 2);
                if bx != 0 && by != 0 {
                    self.draw_preview(srv, &b, item, &mut grid, 2, height + 1, bx, by);
                }
            }
        }
        if n != 0 {
            *self.last_grid.borrow_mut() = Some(grid.clone());
        }
        if self.help {
            self.draw_help(srv, &mut grid, wu, hu, t);
        }
        let mut cursor = None;
        if let Some((_, p)) = &self.prompt {
            let top = srv.opt_session(t.sid, "status-position") == "top";
            let py = if top { 0 } else { hu.saturating_sub(1) };
            if hu != 0 && wu != 0 {
                // mode_tree_draw_prompt: cells the prompt style does not fill stay as drawn
                let (cells, cx) = p.draw(srv, t, self.client, wu, false);
                for (x, c) in cells.into_iter().enumerate() {
                    if let (Some(c), Some(d)) = (c, grid[py].get_mut(x)) {
                        *d = c;
                    }
                }
                cursor = Some((cx as u16, py as u16));
            }
        }
        (grid.into_iter().map(|cells| Line { cells, wrapped: false, ..Default::default() }).collect(), cursor)
    }

    fn view_name(&self) -> Option<&'static str> {
        match self.kind {
            TreeKind::Tree | TreeKind::Clients => Some(if self.info { "info" } else { "preview" }),
            _ => None,
        }
    }

    /// mode_tree_draw for the list part.
    #[allow(clippy::too_many_arguments)]
    fn draw_list(&self, srv: &Server, b: &Built, grid: &mut [Vec<Cell>], w: usize, height: usize, cur: usize, offset: usize, t: Target) {
        let wid = t.wid;
        let mut gc = mode_style(srv, &srv.opt_window(wid, "tree-mode-selection-style"), Style::default(), t);
        let mut gc0 = Style::default();
        let keylen = b.lines.iter().filter_map(|l| l.key.as_ref()).map(|k| k.len() + 3).max().unwrap_or(0);
        let maxdepth = b.lines.iter().map(|l| l.depth).max().unwrap_or(0);
        let mut alignlen = vec![0usize; maxdepth + 1];
        for l in &b.lines {
            let node = &b.nodes[l.item];
            if node.align {
                alignlen[l.depth] = alignlen[l.depth].max(node.name.len());
            }
        }
        let (dfg, dfg0) = (gc.fg, gc0.fg);
        for (i, l) in b.lines.iter().enumerate().skip(offset).take(height) {
            let node = &b.nodes[l.item];
            let row = i - offset;
            // MODE_TREE_PREFIX_FORMAT
            let mut prefix = String::from(PREFIX_STYLE);
            let key = l.key.as_ref().map(|k| format!("({k})")).unwrap_or_default();
            prefix.push_str(&format!("{key:<keylen$}"));
            if l.depth > 0 {
                let parent_last = node.parent.and_then(|p| b.lines.iter().find(|x| x.item == p)).is_some_and(|x| x.last);
                for _ in 0..l.depth - 1 {
                    prefix.push_str(if parent_last { "    " } else { "│   " });
                }
                prefix.push_str(if l.last { "└─→ " } else { "├─→ " });
            }
            if !node.children.is_empty() {
                prefix.push_str(if node.expanded { "#[fg=themered]-" } else { "#[fg=themegreen]+" });
                prefix.push_str(PREFIX_STYLE);
                prefix.push(' ');
            } else if !l.flat {
                prefix.push_str("  ");
            }
            let mut prefix_width = crate::format::width(&strip_styles(&prefix));
            prefix_width = prefix_width.min(w);
            let tag = if node.tagged { "*" } else { "" };
            let separator = if node.text.is_some() { "#[fg=themelightgrey]: #[default]" } else { "" };
            let name = if node.align { format!("{:>width$}", node.name, width = alignlen[l.depth]) } else { node.name.clone() };
            let text = format!("{}{tag}{separator}", name.replace('#', "##"));
            let text_width = crate::format::width(&strip_styles(&text)).min(w - prefix_width);
            let left = w - prefix_width;
            let width = prefix_width + text_width;
            if node.tagged {
                gc.fg = crate::format::parse_color("themecyan").unwrap_or(Color::Default);
                gc0.fg = gc.fg;
            }
            let selected = i == cur;
            let line = &mut grid[row];
            let (pbase, base) = if selected { (gc, gc) } else { (Style::default(), gc0) };
            if selected {
                for c in line.iter_mut() {
                    *c = Cell { text: " ".into(), style: Style { bg: gc.bg, ..Style::default() } };
                }
            }
            let fix = |cells: &mut Vec<Cell>| {
                if selected {
                    for c in cells.iter_mut() {
                        c.style.fg = gc.fg;
                        c.style.bg = gc.bg;
                    }
                }
            };
            let mut cells = format_cells(&prefix, prefix_width, pbase);
            fix(&mut cells);
            put_cells(line, 0, &cells);
            if left != 0 {
                let mut cells = format_cells(&text, left, base);
                fix(&mut cells);
                put_cells(line, prefix_width, &cells[..text_width.min(cells.len())]);
                if let Some(t) = &node.text {
                    if width < w {
                        let mut cells = format_cells(t, w - width, base);
                        fix(&mut cells);
                        put_cells(line, width, &cells);
                    }
                }
            }
            if node.tagged {
                gc.fg = dfg;
                gc0.fg = dfg0;
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_preview(&self, srv: &Server, b: &Built, item: usize, grid: &mut [Vec<Cell>], x: usize, y: usize, sx: usize, sy: usize) {
        let data = &b.nodes[item].data;
        match self.kind {
            TreeKind::Tree => {
                if self.info {
                    self.draw_info(srv, data, grid, x, y, sx, sy);
                    return;
                }
                match data {
                    ItemData::Session(s) => self.draw_strip(srv, grid, x, y, sx, sy, data, *s),
                    ItemData::Window(s, _) => self.draw_strip(srv, grid, x, y, sx, sy, data, *s),
                    ItemData::Pane(_, _, p) => {
                        if !self.hide_this_pane || *p != self.owner {
                            preview_into(srv, *p, grid, x, y, sx, sy);
                        }
                    }
                    _ => {}
                }
            }
            TreeKind::Buffers => {
                if let ItemData::Buffer(n) = data {
                    if let Some(buf) = srv.buffers.iter().find(|b| &b.name == n) {
                        let text = String::from_utf8_lossy(&buf.data);
                        for (i, line) in text.split('\n').take(sy).enumerate() {
                            let vis: String = line.chars().flat_map(|c| if c == '\t' { "\\t".chars().collect::<Vec<_>>() } else if c.is_control() { format!("\\{:03o}", c as u32).chars().collect() } else { vec![c] }).collect();
                            super::put_text(grid, x, y + i, &vis, Style::default(), x + sx);
                        }
                    }
                }
            }
            TreeKind::Clients => {
                if let ItemData::Client(c) = data {
                    let Some(cl) = srv.clients.get(c) else { return };
                    if self.info {
                        // window_client_draw_info: the lines for the client, then the line
                        // between the labels and the values down the rest of the preview
                        let t = Tree::client_target(srv, cl);
                        let ctx = crate::server::vars::VarCtx { srv, t, client: Some(*c), extra: HashMap::from([("clipboard_invalid".to_string(), "0".to_string())]) };
                        let mut i = 0;
                        for l in CLIENT_INFO_LINES.iter().take(sy) {
                            let cells = format_cells(&crate::format::expand(l, &ctx), sx, Style::default());
                            put_cells(&mut grid[y + i], x, &cells);
                            i += 1;
                        }
                        if sx > 14 && i < sy {
                            let gc = mode_style(srv, &srv.opt_window(t.wid, "tree-mode-border-style"), Style::default(), t);
                            for yy in i..sy {
                                if let Some(c) = grid[y + yy].get_mut(x + 14) { *c = Cell { text: "│".into(), style: gc }; }
                            }
                        }
                        return;
                    }
                    // window_client_draw: the active pane (or with -h, when that is this mode's
                    // pane, the last one), a line, then the client's status lines
                    let t = Tree::client_target(srv, cl);
                    let mut pane = t.pane;
                    if self.hide_this_pane && pane == Some(self.owner) {
                        pane = t.wid.and_then(|w| srv.windows.get(&w)).and_then(|w| w.last());
                    }
                    let lines = srv.status_lines(t.sid) as usize;
                    let lines = if lines >= sy { 0 } else { lines };
                    let top = srv.opt_session(t.sid, "status-position") == "top";
                    let at = if top { lines } else { 0 };
                    if let Some(p) = pane {
                        preview_into(srv, p, grid, x, y + at, sx, sy.saturating_sub(2 + lines));
                    }
                    let hy = if at != 0 { y + 2 } else { (y + sy).saturating_sub(1 + lines) };
                    let owner_t = srv.target_for_pane(self.owner);
                    let gc = mode_style(srv, &srv.opt_window(owner_t.wid, "tree-mode-border-style"), Style::default(), owner_t);
                    for xx in 0..sx {
                        if let Some(c) = grid.get_mut(hy).and_then(|r| r.get_mut(x + xx)) {
                            *c = Cell { text: "─".into(), style: gc };
                        }
                    }
                }
            }
            TreeKind::Options => customize::draw(self, srv, b, item, grid, x, y, sx, sy),
        }
    }

    fn strip_current(&self, srv: &Server, data: &ItemData) -> usize {
        match data {
            ItemData::Session(s) => srv.sessions.get(s).and_then(|x| x.links.keys().position(|i| *i == x.active)).unwrap_or(0),
            ItemData::Window(s, i) => srv.sessions.get(s).and_then(|x| x.links.get(i)).and_then(|w| srv.windows.get(w)).map(|w| w.panes().into_iter().filter(|p| !(self.hide_this_pane && *p == self.owner)).position(|p| p == w.active).unwrap_or(0)).unwrap_or(0),
            _ => 0,
        }
    }

    /// The visible range of a list of previews (window_tree_draw_session/window).
    fn strip_geometry(&self, total: usize, current: usize, sx: usize) -> Option<StripGeo> {
        if total == 0 {
            return None;
        }
        let visible = if sx / total < 24 { (sx / 24).max(1) } else { total };
        let (mut start, mut end) = if current < visible {
            (0, visible)
        } else if current >= total - visible {
            (total - visible, total)
        } else {
            let s = current - visible / 2;
            (s, s + visible)
        };
        let mut off = self.preview_offset.get();
        off = off.max(-(start as i32)).min((total - end) as i32);
        self.preview_offset.set(off);
        start = (start as i32 + off) as usize;
        end = (end as i32 + off) as usize;
        let (mut left, mut right) = (start != 0, end != total);
        if (left && right && sx <= 6) || ((left || right) && sx <= 3) {
            left = false;
            right = false;
        }
        let used = if left && right { sx - 6 } else if left || right { sx - 3 } else { sx };
        let each = used / visible;
        if each == 0 {
            return None;
        }
        Some(StripGeo { start, end, each, remaining: used - visible * each, left, right })
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_strip(&self, srv: &Server, grid: &mut [Vec<Cell>], cx: usize, cy: usize, sx: usize, sy: usize, data: &ItemData, sid: SessId) {
        let Some(s) = srv.sessions.get(&sid) else { return };
        // (pane to preview, target for the label, label type)
        let entries: Vec<(PaneId, Target, FmtType)> = match data {
            ItemData::Session(_) => s.links.iter().filter_map(|(i, w)| srv.windows.get(w).map(|win| (win.active, Target { sid: Some(sid), idx: Some(*i), wid: Some(*w), pane: Some(win.active) }, FmtType::Window))).collect(),
            ItemData::Window(_, i) => {
                let Some(win) = s.links.get(i).and_then(|w| srv.windows.get(w)) else { return };
                win.panes().into_iter().filter(|p| !(self.hide_this_pane && *p == self.owner)).map(|p| (p, Target { sid: Some(sid), idx: Some(*i), wid: Some(win.id), pane: Some(p) }, FmtType::Pane)).collect()
            }
            _ => return,
        };
        let Some(geo) = self.strip_geometry(entries.len(), self.strip_current(srv, data), sx) else { return };
        let owner_t = srv.target_for_pane(self.owner);
        let gc = mode_style(srv, &srv.opt_window(owner_t.wid, "tree-mode-border-style"), Style::default(), owner_t);
        let vline = |grid: &mut [Vec<Cell>], x: usize| {
            for yy in 0..sy {
                if let Some(c) = grid.get_mut(cy + yy).and_then(|r| r.get_mut(x)) {
                    *c = Cell { text: "│".into(), style: gc };
                }
            }
        };
        if geo.left {
            vline(grid, cx + 2);
            super::put_text(grid, cx, cy + sy / 2, "<", gc, cx + 1);
        }
        if geo.right {
            vline(grid, cx + sx - 3);
            super::put_text(grid, cx + sx - 1, cy + sy / 2, ">", gc, cx + sx);
        }
        for (i, (pane, t, ty)) in entries.iter().enumerate().take(geo.end).skip(geo.start) {
            let k = i - geo.start;
            let offset = if geo.left { 3 + k * geo.each } else { k * geo.each };
            let width = if i == geo.end - 1 { geo.each + geo.remaining } else { geo.each - 1 };
            preview_into(srv, *pane, grid, cx + offset, cy, width, sy);
            let opts_wid = t.wid;
            let border = mode_style(srv, &srv.opt_window(opts_wid, "tree-mode-border-style"), Style::default(), *t);
            let mut label_gc = mode_style(srv, &srv.opt_window(opts_wid, "tree-mode-preview-style"), Style::default(), *t);
            label_gc.bg = border.bg;
            let format = match ty {
                FmtType::Pane => srv.opt_pane(*pane, "tree-mode-preview-format"),
                _ => srv.opt_window(opts_wid, "tree-mode-preview-format"),
            };
            if !format.is_empty() {
                let label = expand_as(srv, *t, *ty, &format, HashMap::new());
                if !label.is_empty() {
                    draw_label(grid, cx + offset, cy, width, sy, border, label_gc, &label);
                }
            }
            if i != geo.end - 1 {
                vline(grid, cx + offset + width);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_info(&self, srv: &Server, data: &ItemData, grid: &mut [Vec<Cell>], cx: usize, cy: usize, sx: usize, sy: usize) {
        let pane_lines = [
            info_line("Pane", "#{pane_index} #[fg=themelightgrey](#{pane_id})#[default]"),
            info_line("Title", "#{pane_title}"),
            info_line("Command", "#{pane_current_command} #[fg=themelightgrey](PID #{pane_pid})#[default]"),
            info_line("Path", "#{pane_current_path}"),
            info_line("TTY", "#{pane_tty}"),
            info_line("Position", "#{pane_x},#{pane_y} #{pane_width}x#{pane_height}"),
            info_line("Mode", "#{?pane_in_mode,#{pane_mode},none}"),
            info_line("Flags", &[flag("active", "pane_active"), flag("zoomed", "window_zoomed_flag"), flag("marked", "pane_marked"), flag("sync", "pane_synchronized"), flag("dead", "pane_dead"), flag("piped", "pane_pipe")].join(" ")),
        ];
        let window_lines = [
            info_line("Window", "#{window_index}: #{window_name} #[fg=themelightgrey](#{window_id})#[default]"),
            info_line("Size", "#{window_width}x#{window_height}"),
            info_line("Panes", "#{window_panes}"),
            info_line("Activity Time", "#{t:window_activity} #[fg=themelightgrey](#{t/r:window_activity})#[default]"),
            info_line("Sessions", "#{s/,/ /:window_linked_sessions_list}"),
            info_line("Flags", &[flag("active", "window_active"), flag("last", "window_last_flag"), flag("bell", "window_bell_flag"), flag("activity", "window_activity_flag"), flag("silence", "window_silence_flag"), flag("zoomed", "window_zoomed_flag"), flag("marked", "window_marked_flag")].join(" ")),
        ];
        let session_lines = [
            info_line("Session", "#{session_name} #[fg=themelightgrey](#{session_id})#[default]"),
            info_line("Created Time", "#{t:session_created} #[fg=themelightgrey](#{t/r:session_created})#[default]"),
            info_line("Activity Time", "#{t:session_activity} #[fg=themelightgrey](#{t/r:session_activity})#[default]"),
            info_line("Attached Time", "#{?#{t:session_last_attached},#{t:session_last_attached} #[fg=themelightgrey](#{t/r:session_last_attached})#[default],never}"),
            info_line("Clients", "#{s/,/ /:session_attached_list}"),
            info_line("Windows", "#{session_windows}"),
            info_line("Path", "#{session_path}"),
            info_line("Group", "#{?session_grouped,#{session_group} (#{session_group_size}),none}"),
            info_line("Flags", &[flag("attached", "session_attached"), flag("grouped", "session_grouped"), flag("marked", "session_marked"), flag("bell", "session_bell_flag"), flag("activity", "session_activity_flag"), flag("silence", "session_silence_flag")].join(" ")),
        ];
        let (t, _) = self.item_target(srv, data);
        if t.sid.is_none() || t.pane.is_none() {
            return;
        }
        let mut groups: Vec<&[String]> = Vec::new();
        if matches!(data, ItemData::Pane(..)) {
            groups.push(&pane_lines);
        }
        if matches!(data, ItemData::Pane(..) | ItemData::Window(..)) {
            groups.push(&window_lines);
        }
        groups.push(&session_lines);
        let owner_t = srv.target_for_pane(self.owner);
        let gc = mode_style(srv, &srv.opt_window(owner_t.wid, "tree-mode-border-style"), Style::default(), owner_t);
        let mut i = 0;
        'outer: for (j, g) in groups.iter().enumerate() {
            if j != 0 {
                if i == sy {
                    break;
                }
                for xx in 0..sx {
                    if let Some(c) = grid[cy + i].get_mut(cx + xx) { *c = Cell { text: if xx == 14 { "┼" } else { "─" }.into(), style: gc }; }
                }
                i += 1;
            }
            for l in g.iter() {
                if i == sy {
                    break 'outer;
                }
                let text = expand_as(srv, t, FmtType::None, &l.replace("#{E:tree-mode-border-style}", &srv.opt_window(owner_t.wid, "tree-mode-border-style")), HashMap::new());
                let cells = format_cells(&text, sx, Style::default());
                put_cells(&mut grid[cy + i], cx, &cells);
                i += 1;
            }
        }
        if sx > 14 {
            for yy in i..sy {
                if let Some(c) = grid[cy + yy].get_mut(cx + 14) { *c = Cell { text: "│".into(), style: gc }; }
            }
        }
    }

    /// mode_tree_draw_help: a box of the keys in the middle of the mode.
    fn draw_help(&self, srv: &Server, grid: &mut [Vec<Cell>], sx: usize, sy: usize, t: Target) {
        let (extra, width, item): (&[(&str, &str)], usize, &str) = match self.kind {
            TreeKind::Tree => (HELP_TREE, 51, "item"),
            TreeKind::Buffers => (HELP_BUFFER, 0, "buffer"),
            TreeKind::Clients => (HELP_CLIENT, 0, "client"),
            TreeKind::Options => (customize::HELP, 52, "item"),
        };
        let w = width.max(39);
        let lines: Vec<&(&str, &str)> = HELP_START.iter().chain(extra.iter()).chain(HELP_END.iter()).collect();
        let (box_w, box_h) = (w + 2, lines.len() + 2);
        if sx < box_w || sy < box_h {
            return;
        }
        let (x, y) = ((sx - box_w) / 2, (sy - box_h) / 2);
        let border_opt = srv.opt_window(t.wid, "tree-mode-border-style");
        let border = mode_style(srv, &border_opt, Style::default(), t);
        super::draw_box(grid, x, y, box_w, box_h, border);
        for (i, (k, d)) in lines.iter().enumerate() {
            let line = format!("#[fg=themelightgrey]{k} #[{border_opt}]│#[default] {}", d.replace("%1", item));
            let text = expand_as(srv, t, FmtType::None, &line, HashMap::new());
            let row = &mut grid[y + 1 + i];
            for c in row.iter_mut().skip(x + 1).take(w) {
                *c = Cell::default();
            }
            put_cells(row, x + 1, &format_cells(&text, w, Style::default()));
        }
    }
}

/// window_client_info_lines (choose-client -i), with the ACS line characters as box
/// drawing characters.
pub const CLIENT_INFO_LINES: &[&str] = &[
    "#[fg=themelightgrey]Client Name   #[#{E:tree-mode-border-style},acs]│#[default] #{client_name} #[fg=themelightgrey]#[fg=themelightgrey](PID #{client_pid})#[default]",
    "#[fg=themelightgrey]Session       #[#{E:tree-mode-border-style},acs]│#[default] #{session_name}",
    "#[fg=themelightgrey]Attach Time   #[#{E:tree-mode-border-style},acs]│#[default] #{t:client_created} #[fg=themelightgrey](#{t/r:client_created})#[default]",
    "#[fg=themelightgrey]Activity Time #[#{E:tree-mode-border-style},acs]│#[default] #{t:client_activity} #[fg=themelightgrey](#{t/r:client_activity})#[default]",
    "#[fg=themelightgrey]Terminal Type #[#{E:tree-mode-border-style},acs]│#[default] #{?client_termtype,#{client_termtype},Unknown}",
    "#[fg=themelightgrey]TERM          #[#{E:tree-mode-border-style},acs]│#[default] #{client_termname}",
    "#[fg=themelightgrey]Size          #[#{E:tree-mode-border-style},acs]│#[default] #{client_width}x#{client_height} #[fg=themelightgrey](cell #{client_cell_width}x#{client_cell_height})#[default]",
    "#[fg=themelightgrey]Bytes Written #[#{E:tree-mode-border-style},acs]│#[default] #{client_written} #[fg=themelightgrey](#{client_discarded} discarded)#[default]",
    "#[fg=themelightgrey]Features      #[#{E:tree-mode-border-style},acs]│#[default] #{?#{I/f:256},#[fg=themegreen],#[fg=themelightgrey]}#{p/15:#{l:256}}#[default] #{?#{I/f:RGB},#[fg=themegreen],#[fg=themelightgrey]}#{p/15:#{l:RGB}}#[default] #{?#{I/f:bpaste},#[fg=themegreen],#[fg=themelightgrey]}#{p/15:#{l:bpaste}}#[default] #{?#{I/f:ccolour},#[fg=themegreen],#[fg=themelightgrey]}#{p/15:#{l:ccolour}}#[default]",
    "              #[#{E:tree-mode-border-style},acs]│#[default] #{?#{I/f:clipboard},#[fg=themegreen],#[fg=themelightgrey]}#{p/15:#{l:clipboard}}#[default] #{?#{I/f:cstyle},#[fg=themegreen],#[fg=themelightgrey]}#{p/15:#{l:cstyle}}#[default] #{?#{I/f:extkeys},#[fg=themegreen],#[fg=themelightgrey]}#{p/15:#{l:extkeys}}#[default] #{?#{I/f:focus},#[fg=themegreen],#[fg=themelightgrey]}#{p/15:#{l:focus}}#[default]",
    "              #[#{E:tree-mode-border-style},acs]│#[default] #{?#{I/f:hyperlinks},#[fg=themegreen],#[fg=themelightgrey]}#{p/15:#{l:hyperlinks}}#[default] #{?#{I/f:ignorefkeys},#[fg=themegreen],#[fg=themelightgrey]}#{p/15:#{l:ignorefkeys}}#[default] #{?#{I/f:margins},#[fg=themegreen],#[fg=themelightgrey]}#{p/15:#{l:margins}}#[default] #{?#{I/f:mouse},#[fg=themegreen],#[fg=themelightgrey]}#{p/15:#{l:mouse}}#[default]",
    "              #[#{E:tree-mode-border-style},acs]│#[default] #{?#{I/f:osc7},#[fg=themegreen],#[fg=themelightgrey]}#{p/15:#{l:osc7}}#[default] #{?#{I/f:overline},#[fg=themegreen],#[fg=themelightgrey]}#{p/15:#{l:overline}}#[default] #{?#{I/f:progressbar},#[fg=themegreen],#[fg=themelightgrey]}#{p/15:#{l:progressbar}}#[default] #{?#{I/f:rectfill},#[fg=themegreen],#[fg=themelightgrey]}#{p/15:#{l:rectfill}}#[default]",
    "              #[#{E:tree-mode-border-style},acs]│#[default] #{?#{I/f:sixel},#[fg=themegreen],#[fg=themelightgrey]}#{p/15:#{l:sixel}}#[default] #{?#{I/f:strikethrough},#[fg=themegreen],#[fg=themelightgrey]}#{p/15:#{l:strikethrough}}#[default] #{?#{I/f:sync},#[fg=themegreen],#[fg=themelightgrey]}#{p/15:#{l:sync}}#[default] #{?#{I/f:title},#[fg=themegreen],#[fg=themelightgrey]}#{p/15:#{l:title}}#[default]",
    "              #[#{E:tree-mode-border-style},acs]│#[default] #{?#{I/f:usstyle},#[fg=themegreen],#[fg=themelightgrey]}#{p/15:#{l:usstyle}}#[default] #{?#{I/f:utf8},#[fg=themegreen],#[fg=themelightgrey]}#{p/15:#{l:utf8}}#[default]",
    "#[#{E:tree-mode-border-style},acs]──────────────┼#{R:─,#{window_width}}#[default]",
    "#[fg=themelightgrey]prefix        #[#{E:tree-mode-border-style},acs]│#[default] #{prefix}",
    "#[fg=themelightgrey]mouse         #[#{E:tree-mode-border-style},acs]│#[default] #{?mouse,#{?#{I/c:kmous},,#[fg=themered]}on,#[fg=themelightgrey]off} #{?#{I/c:kmous},,#[align=right]unavailable: [kmous] missing}",
    "#[fg=themelightgrey]set-clipboard #[#{E:tree-mode-border-style},acs]│#[default] #{?#{!=:#{set-clipboard},off},#{?#{I/c:Ms},,#[fg=themered]}#{set-clipboard},#[fg=themelightgrey]off} #{?#{I/c:Ms},,#[align=right]unavailable: [Ms] #{?clipboard_invalid,invalid,missing}}",
    "#[fg=themelightgrey]get-clipboard #[#{E:tree-mode-border-style},acs]│#[default] #{?#{!=:#{get-clipboard},off},#{?#{I/c:Ms},,#[fg=themered]}#{get-clipboard},#[fg=themelightgrey]off} #{?#{I/c:Ms},,#[align=right]unavailable: [Ms] #{?clipboard_invalid,invalid,missing}}",
    "#[fg=themelightgrey]focus-events  #[#{E:tree-mode-border-style},acs]│#[default] #{?focus-events,#{?#{I/f:focus},,#[fg=themered]}on,#[fg=themelightgrey]off} #{?#{I/f:focus},,#[align=right]unavailable: [Enfcs] or [Dcfcs] missing}",
    "#[fg=themelightgrey]extended-keys #[#{E:tree-mode-border-style},acs]│#[default] #{?#{!=:#{extended-keys},off},#{?#{I/f:extkeys},,#[fg=themered]}#{extended-keys},#[fg=themelightgrey]off} #{?#{I/f:extkeys},,#[align=right]unavailable: [Eneks] or [Dseks] missing}",
    "#[fg=themelightgrey]set-titles    #[#{E:tree-mode-border-style},acs]│#[default] #{?set-titles,on,#[fg=themelightgrey]off}",
    "#[fg=themelightgrey]escape-time   #[#{E:tree-mode-border-style},acs]│#[default] #{escape-time} ms",
];

struct StripGeo {
    start: usize,
    end: usize,
    each: usize,
    remaining: usize,
    left: bool,
    right: bool,
}

/// Text without its #[...] styles (and with ## as #), for measuring.
pub fn strip_styles(s: &str) -> String {
    crate::format::spans(s, Style::default()).into_iter().map(|sp| sp.text).collect()
}

/// window_tree_draw_label: a boxed label in the middle of a preview.
#[allow(clippy::too_many_arguments)]
fn draw_label(grid: &mut [Vec<Cell>], px: usize, py: usize, sx: usize, sy: usize, border: Style, label_gc: Style, label: &str) {
    if sx < 5 || sy < 3 {
        return;
    }
    let mut cells = format_cells(label, crate::format::width(&strip_styles(label)), label_gc);
    if cells.len() > sx - 4 {
        cells.truncate(sx - 4);
    }
    let width = cells.len();
    if width == 0 {
        return;
    }
    let ox = (sx - width + 1) / 2;
    let oy = (sy + 1) / 2;
    super::draw_box(grid, px + ox - 2, py + oy - 1, width + 4, 3, border);
    let blank = Cell { text: " ".into(), style: Style { bg: border.bg, ..Style::default() } };
    for k in 0..width + 2 {
        if let Some(c) = grid.get_mut(py + oy).and_then(|r| r.get_mut(px + ox - 1 + k)) {
            *c = blank.clone();
        }
    }
    put_cells(&mut grid[py + oy], px + ox, &cells);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn built(n: usize) -> Built {
        let nodes: Vec<Node> = (0..n)
            .map(|i| Node { tag: format!("t{i}"), name: i.to_string(), text: None, data: ItemData::Buffer(i.to_string()), parent: None, children: vec![], expanded: false, tagged: false, align: false, no_tag: false, draw_as_parent: false })
            .collect();
        let lines = (0..n).map(|i| TLine { item: i, depth: 0, last: i + 1 == n, flat: true, key: None }).collect();
        Built { nodes, roots: (0..n).collect(), lines, no_matches: false }
    }

    #[test]
    fn rebuilding_keeps_the_scroll_position() {
        let mut t = Tree::new(TreeKind::Buffers, 0, Target::default());
        let b = built(30);
        // scrolled so line 12 is shown at the top with line 15 current
        t.offset = 12;
        t.cur_tag = Some("t15".into());
        assert_eq!(t.resolve(&b, 10), (15, 12));
        // the current item moved up after a rebuild: the offset stays while it is visible
        let mut b2 = built(30);
        b2.nodes.swap(15, 13);
        b2.nodes[13].tag = "t15".into();
        b2.nodes[15].tag = "t13".into();
        assert_eq!(t.resolve(&b2, 10), (13, 12));
        // the list shrank: no empty lines at the bottom
        assert_eq!(t.resolve(&built(16), 10), (15, 6));
        // the current item went away: its line, clamped to the list
        t.cur_tag = Some("gone".into());
        t.cur = 40;
        assert_eq!(t.resolve(&built(20), 10).0, 19);
    }

    #[test]
    fn list_height_follows_the_preview() {
        let mut t = Tree::new(TreeKind::Tree, 0, Target::default());
        assert_eq!(t.list_height(24, 9), 12);
        assert_eq!(t.list_height(24, 40), 16);
        assert_eq!(t.list_height(12, 40), 12);
        t.preview = Preview::Big;
        assert_eq!(t.list_height(24, 40), 6);
        t.preview = Preview::Off;
        assert_eq!(t.list_height(24, 40), 24);
        let c = Tree::new(TreeKind::Options, 0, Target::default());
        assert_eq!(c.list_height(30, 5), 18);
    }

    #[test]
    fn sort_orders_cycle_per_mode() {
        let t = Tree::new(TreeKind::Buffers, 0, Target::default());
        assert_eq!(t.sort(), Some(Sort::Creation));
        assert_eq!(Tree::new(TreeKind::Clients, 0, Target::default()).sort(), Some(Sort::Name));
        assert_eq!(Tree::new(TreeKind::Options, 0, Target::default()).sort(), None);
        assert_eq!(Sort::parse("Z"), Some(Sort::Z));
        assert_eq!(Sort::parse("modifier"), Some(Sort::Modifier));
        assert_eq!(Sort::parse("Order"), Some(Sort::Order));
        assert_eq!(strip_styles("#[fg=red]a##b#[default]c"), "a#bc");
        // -O outside the O cycle is kept, and O starts the cycle again from it
        let mut t = Tree::new(TreeKind::Tree, 0, Target::default());
        t.order = Some(Sort::Creation);
        assert_eq!(t.sort(), Some(Sort::Creation));
        let mut v = vec![3, 1, 2];
        sort_by(&mut v, Sort::Order, true, |a, b| a.cmp(b));
        assert_eq!(v, [2, 1, 3]);
        sort_by(&mut v, Sort::Size, false, |a, b| a.cmp(b));
        assert_eq!(v, [1, 2, 3]);
    }
}
