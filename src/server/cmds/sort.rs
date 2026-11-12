//! Sort orders for the list commands (`-O order` and `-r`), after tmux's sort.c.

use std::cmp::Ordering;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Order {
    Activity,
    Creation,
    Index,
    Modifier,
    Name,
    Order,
    Size,
    Z,
}

/// `-O` names (case-insensitive; "key" = index, "title" = name), like sort_order_from_string.
pub fn parse(s: &str) -> Option<Order> {
    Some(match s.to_ascii_lowercase().as_str() {
        "activity" => Order::Activity,
        "creation" => Order::Creation,
        "index" | "key" => Order::Index,
        "modifier" => Order::Modifier,
        "name" | "title" => Order::Name,
        "order" => Order::Order,
        "size" => Order::Size,
        "z" => Order::Z,
        _ => return None,
    })
}

/// The `-O`/`-r` criteria of a command: no `-O` keeps the natural order (and then `-r` does
/// nothing either, as in tmux).
#[derive(Clone, Copy, Debug)]
pub struct Criteria {
    pub order: Option<Order>,
    pub reversed: bool,
}

impl Criteria {
    pub fn from_args(order: Option<&str>, reversed: bool) -> Result<Criteria, String> {
        let order = match order {
            Some(o) => Some(parse(o).ok_or("invalid sort order")?),
            None => None,
        };
        Ok(Criteria { order, reversed })
    }

    /// Sorts `v` with `cmp` (which includes its own tie-breaks); `order` only reverses.
    pub fn sort<T>(&self, v: &mut [T], cmp: impl Fn(&T, &T, Order) -> Ordering) {
        let Some(order) = self.order else { return };
        if v.len() < 2 {
            return;
        }
        if order == Order::Order {
            if self.reversed {
                v.reverse();
            }
            return;
        }
        v.sort_by(|a, b| {
            let r = cmp(a, b, order);
            if self.reversed { r.reverse() } else { r }
        });
    }
}

/// tmux's key code for a key name, for sorting bindings by key ("index") or modifier.
/// Unicode keys are their code point, special keys follow in tmux's enum order, then mouse
/// keys by type, button and location; modifiers sit in the high bits.
pub fn key_code(name: &str) -> u64 {
    const META: u64 = 0x0010_0000_0000_00;
    const CTRL: u64 = 0x0020_0000_0000_00;
    const SHIFT: u64 = 0x0040_0000_0000_00;
    let mut mods = 0;
    let mut rest = name;
    while rest.len() >= 3 && rest.as_bytes()[1] == b'-' {
        match rest.as_bytes()[0] {
            b'C' | b'c' => mods |= CTRL,
            b'M' | b'm' => mods |= META,
            b'S' | b's' => mods |= SHIFT,
            _ => break,
        }
        rest = &rest[2..];
    }
    let ty = |t: u64| t << 32;
    let function = ty(2);
    let mut chars = rest.chars();
    let base = match (chars.next(), chars.next()) {
        (Some(c), None) => c as u64,
        _ => match rest {
            "Space" => b' ' as u64,
            "Tab" => 9,
            "Enter" => 13,
            "Escape" => 27,
            "Any" => function + 4,
            "BSpace" => function + 7,
            "IC" | "Insert" => function + 20,
            "DC" | "Delete" => function + 21,
            "Home" => function + 22,
            "End" => function + 23,
            "NPage" | "PageDown" | "PgDn" => function + 24,
            "PPage" | "PageUp" | "PgUp" => function + 25,
            "BTab" => function + 26,
            "Up" => function + 27,
            "Down" => function + 28,
            "Left" => function + 29,
            "Right" => function + 30,
            _ => special_code(rest).unwrap_or(u64::MAX >> 8),
        },
    };
    base | mods
}

fn special_code(name: &str) -> Option<u64> {
    let function = 2u64 << 32;
    if let Some(n) = name.strip_prefix('F').and_then(|n| n.parse::<u64>().ok()).filter(|n| (1..=12).contains(n)) {
        return Some(function + 7 + n);
    }
    const KP: [&str; 16] = ["KP/", "KP*", "KP-", "KP7", "KP8", "KP9", "KP+", "KP4", "KP5", "KP6", "KP1", "KP2", "KP3", "KPEnter", "KP0", "KP."];
    if let Some(i) = KP.iter().position(|k| *k == name) {
        return Some(function + 31 + i as u64);
    }
    if let Some(n) = name.strip_prefix("User").and_then(|n| n.parse::<u64>().ok()) {
        return Some((1u64 << 32) + n);
    }
    // mouse keys: type, button and location (tmux.h KEYC_MAKE_MOUSE_KEY)
    const TYPES: [(&str, u64); 10] = [
        ("MouseMove", 3),
        ("MouseDragEnd", 7),
        ("MouseDown", 4),
        ("MouseUp", 5),
        ("MouseDrag", 6),
        ("WheelDown", 8),
        ("WheelUp", 9),
        ("SecondClick", 10),
        ("DoubleClick", 11),
        ("TripleClick", 12),
    ];
    const LOCATIONS: [&str; 10] = ["Pane", "Status", "StatusLeft", "StatusRight", "StatusDefault", "Border", "ScrollbarUp", "ScrollbarSlider", "ScrollbarDown", "Empty"];
    let (rest, t) = TYPES.iter().find_map(|(p, t)| name.strip_prefix(p).map(|r| (r, *t)))?;
    let digits = rest.chars().take_while(|c| c.is_ascii_digit()).count();
    let button: u64 = rest[..digits].parse().unwrap_or(0);
    let loc = &rest[digits..];
    let location = match loc.strip_prefix("Control").and_then(|n| n.parse::<u64>().ok()) {
        Some(n) => 10 + n,
        None => LOCATIONS.iter().position(|l| *l == loc)? as u64,
    };
    Some((t << 32) | (button << 8) | location)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orders_and_keys() {
        assert_eq!(parse("Title"), Some(Order::Name));
        assert_eq!(parse("key"), Some(Order::Index));
        assert!(Criteria::from_args(Some("bogus"), false).is_err());
        let mut v = vec![3, 1, 2];
        Criteria::from_args(None, true).unwrap().sort(&mut v, |a, b, _| a.cmp(b));
        assert_eq!(v, [3, 1, 2], "-r alone keeps the natural order");
        Criteria::from_args(Some("order"), true).unwrap().sort(&mut v, |a, b, _| a.cmp(b));
        assert_eq!(v, [2, 1, 3]);
        Criteria::from_args(Some("name"), true).unwrap().sort(&mut v, |a, b, _| a.cmp(b));
        assert_eq!(v, [3, 2, 1]);
        // tmux's prefix table order: Tab, Space, punctuation, letters, specials, then modified
        let mut keys = vec!["C-o", "Up", "M-1", "a", "Space", "Tab", "!", "PPage", "F1", "MouseDown1Pane", "C-Up"];
        keys.sort_by_key(|k| key_code(k));
        assert_eq!(keys, ["Tab", "Space", "!", "a", "F1", "PPage", "Up", "MouseDown1Pane", "M-1", "C-o", "C-Up"]);
        assert!(key_code("WheelUpPane") > key_code("MouseDragEnd1Pane"));
        assert!(key_code("DoubleClick1Pane") < key_code("TripleClick1Pane"));
    }
}
