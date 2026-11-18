//! A deterministic "template draft": a sensible room graph built from the words of a prompt,
//! with no model at all (plan 3.6). It is what the studio offers when no AI is set up, and it
//! is a floor under every AI path: whatever is typed, the answer is a valid, linted MapSpec.
//! It reads a setting (crypt, tower, ship, station, tavern, mine, or a plain dungeon), the
//! number of floors ("3 levels"), whether something is secret, a size word, and any room
//! names the furnisher knows (a "kitchen" in the prompt becomes a kitchen).

use crate::gen::Rng;
use crate::grid::SpaceKind;
use crate::mapspec::{
    compile, FloorSpec, LinkKind, LinkSpec, MapSpec, RoomSpec, StairSpec, MAX_ROOMS,
};

/// At most this many floors come from a prompt.
pub const MAX_TEMPLATE_FLOORS: usize = 4;

struct Setting {
    keys: &'static [&'static str],
    title: &'static str,
    floor_word: &'static str,
    entry: &'static str,
    hub: &'static str,
    /// Rooms to draw from (name, width, height); names reuse the furnisher's keywords.
    rooms: &'static [(&'static str, i32, i32)],
    secret: &'static str,
}

const SETTINGS: &[Setting] = &[
    Setting {
        keys: &["crypt", "tomb", "grave", "catacomb", "mausoleum", "ossuary"],
        title: "Crypt",
        floor_word: "Crypt Level",
        entry: "Crypt Entrance",
        hub: "Burial Hall",
        rooms: &[
            ("Ossuary", 6, 5),
            ("Burial Vault", 7, 6),
            ("Chapel", 7, 6),
            ("Tomb", 6, 6),
            ("Prison Cell", 4, 4),
            ("Storage Pantry", 5, 4),
            ("Altar Room", 6, 5),
            ("Guard Quarters", 7, 5),
        ],
        secret: "Hidden Tomb",
    },
    Setting {
        keys: &["tower", "wizard", "mage", "keep"],
        title: "Tower",
        floor_word: "Tower Floor",
        entry: "Ground Hall",
        hub: "Great Hall",
        rooms: &[
            ("Library", 7, 6),
            ("Study", 5, 5),
            ("Armory", 6, 5),
            ("Store Room", 5, 4),
            ("Bedroom Quarters", 7, 5),
            ("Workshop", 6, 6),
            ("Archive", 6, 5),
            ("Kitchen", 6, 5),
        ],
        secret: "Hidden Study",
    },
    Setting {
        keys: &["ship", "vessel", "galleon", "boat", "airship", "frigate"],
        title: "Ship",
        floor_word: "Deck",
        entry: "Gangway",
        hub: "Main Deck",
        rooms: &[
            ("Galley", 6, 4),
            ("Crew Quarters", 8, 5),
            ("Cargo Hold", 9, 6),
            ("Mess", 6, 5),
            ("Captain Quarters", 6, 5),
            ("Brig Cell", 4, 4),
            ("Locker Room", 5, 4),
            ("Workshop", 6, 5),
        ],
        secret: "Smuggler Hold",
    },
    Setting {
        keys: &[
            "station",
            "metro",
            "spaceport",
            "reactor",
            "facility",
            "bunker",
            "lab",
        ],
        title: "Station",
        floor_word: "Level",
        entry: "Entrance Hall",
        hub: "Concourse",
        rooms: &[
            ("Control Office", 6, 5),
            ("Cargo Bay", 9, 6),
            ("Mess", 6, 5),
            ("Lab", 7, 5),
            ("Crew Dorm", 7, 5),
            ("Locker Room", 5, 4),
            ("Workshop", 6, 6),
            ("Archive", 6, 5),
        ],
        secret: "Sealed Vault",
    },
    Setting {
        keys: &["tavern", "inn", "pub", "alehouse", "bar"],
        title: "Tavern",
        floor_word: "Floor",
        entry: "Front Door",
        hub: "Common Hall",
        rooms: &[
            ("Kitchen", 6, 5),
            ("Pantry", 4, 4),
            ("Bedroom", 5, 4),
            ("Guest Quarters", 7, 5),
            ("Office", 5, 4),
            ("Store Room", 5, 5),
            ("Cellar Locker", 5, 4),
            ("Stable Workshop", 7, 5),
        ],
        secret: "Smuggler Cellar",
    },
    Setting {
        keys: &["mine", "cave", "cavern", "quarry", "tunnel"],
        title: "Mine",
        floor_word: "Mine Level",
        entry: "Mine Entrance",
        hub: "Main Cavern",
        rooms: &[
            ("Workshop", 6, 5),
            ("Store Room", 5, 5),
            ("Crew Quarters", 7, 5),
            ("Forge", 6, 6),
            ("Cell", 4, 4),
            ("Locker Room", 5, 4),
            ("Ore Cargo Bay", 8, 6),
            ("Shrine", 5, 5),
        ],
        secret: "Hidden Vault",
    },
    Setting {
        keys: &[],
        title: "Dungeon",
        floor_word: "Level",
        entry: "Entrance Hall",
        hub: "Great Hall",
        rooms: &[
            ("Guard Quarters", 7, 5),
            ("Armory", 6, 5),
            ("Prison Cell", 4, 4),
            ("Shrine", 6, 5),
            ("Store Room", 5, 4),
            ("Kitchen", 6, 5),
            ("Library", 6, 5),
            ("Throne Room", 8, 7),
        ],
        secret: "Hidden Vault",
    },
];

fn words(prompt: &str) -> Vec<String> {
    prompt
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect()
}

fn number_word(w: &str) -> Option<usize> {
    Some(match w {
        "one" | "single" => 1,
        "two" | "double" => 2,
        "three" | "triple" => 3,
        "four" => 4,
        "five" | "six" | "seven" | "eight" | "nine" | "ten" => 4,
        _ => return w.parse::<usize>().ok(),
    })
}

/// How many floors a prompt asks for ("3 levels", "two floors"), 1..=4.
pub fn floors_asked(prompt: &str) -> usize {
    floors_in(&words(prompt))
}

fn floors_in(ws: &[String]) -> usize {
    const UNITS: &[&str] = &[
        "floor", "floors", "level", "levels", "storey", "storeys", "story", "stories", "deck",
        "decks", "tier", "tiers",
    ];
    for (i, w) in ws.iter().enumerate() {
        if UNITS.contains(&w.as_str()) && i > 0 {
            if let Some(n) = number_word(&ws[i - 1]) {
                return n.clamp(1, MAX_TEMPLATE_FLOORS);
            }
        }
        // "3-level" arrives as "3", "level"; "multi-level" asks for a few.
        if w == "multi" && ws.get(i + 1).is_some_and(|n| UNITS.contains(&n.as_str())) {
            return 2;
        }
    }
    1
}

fn title_of(ws: &[String], fallback: &str) -> String {
    let t: Vec<String> = ws
        .iter()
        .filter(|w| w.len() > 1 && !w.chars().all(|c| c.is_ascii_digit()))
        .take(4)
        .map(|w| {
            let mut c = w.chars();
            match c.next() {
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                None => String::new(),
            }
        })
        .collect();
    let t = t.join(" ");
    let t: String = t.chars().take(60).collect();
    if t.trim().is_empty() {
        fallback.to_string()
    } else {
        t
    }
}

fn room(id: &str, name: &str, w: i32, h: i32) -> RoomSpec {
    RoomSpec {
        id: id.into(),
        name: name.into(),
        kind: SpaceKind::Room,
        w,
        h,
        lit: true,
    }
}

fn link(a: &str, b: &str, kind: LinkKind) -> LinkSpec {
    LinkSpec {
        a: a.into(),
        b: b.into(),
        kind,
    }
}

fn build(prompt: &str, seed: u64) -> MapSpec {
    let ws = words(prompt);
    let set = SETTINGS
        .iter()
        .find(|s| ws.iter().any(|w| s.keys.contains(&w.as_str())))
        .unwrap_or(SETTINGS.last().expect("settings"));
    let floors_n = floors_in(&ws);
    let secret = ws
        .iter()
        .any(|w| matches!(w.as_str(), "secret" | "hidden" | "concealed" | "secrets"));
    let big = ws
        .iter()
        .any(|w| matches!(w.as_str(), "large" | "big" | "huge" | "sprawling" | "vast"));
    let small = ws
        .iter()
        .any(|w| matches!(w.as_str(), "small" | "tiny" | "little" | "cramped"));
    let mut rng = Rng(seed ^ 0x7E3F_91A5_5C21_0D4B);
    // Rooms the prompt names that the furnisher knows ("a kitchen and a library").
    let mut asked: Vec<(String, i32, i32)> = Vec::new();
    for (group, keys) in crate::furnish::KEYWORDS {
        if ws.iter().any(|w| keys.contains(&w.as_str()) || w == group) {
            let name: String = {
                let mut c = group.chars();
                c.next()
                    .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
                    .unwrap_or_default()
            };
            asked.push((name, 6, 5));
        }
    }
    let per_floor = if small {
        3
    } else if big {
        8
    } else {
        5
    };
    let mut floors = Vec::new();
    let names: Vec<String> = (0..floors_n)
        .map(|i| {
            if floors_n == 1 {
                format!("{} Floor", set.title)
            } else {
                format!("{} {}", set.floor_word, i + 1)
            }
        })
        .collect();
    for (fi, fname) in names.iter().enumerate() {
        let mut rooms = vec![
            room(
                "entry",
                if fi == 0 { set.entry } else { "Landing" },
                rng.range(5, 7),
                rng.range(4, 6),
            ),
            room("hub", set.hub, rng.range(8, 10), rng.range(6, 8)),
        ];
        let mut links = vec![link("entry", "hub", LinkKind::Opening)];
        // The themed rooms: those asked for first, then a seeded shuffle of the setting's own.
        let mut pool: Vec<(String, i32, i32)> = asked.clone();
        let mut own: Vec<(String, i32, i32)> = set
            .rooms
            .iter()
            .map(|(n, w, h)| (n.to_string(), *w, *h))
            .collect();
        for i in (1..own.len()).rev() {
            own.swap(i, (rng.next() % (i as u64 + 1)) as usize);
        }
        pool.extend(own);
        let mut seen_names: Vec<String> = Vec::new();
        let mut made = 0usize;
        for (name, w, h) in pool {
            if made >= per_floor || rooms.len() + 2 > MAX_ROOMS {
                break;
            }
            if seen_names.contains(&name) {
                continue;
            }
            seen_names.push(name.clone());
            made += 1;
            let id = format!("r{made}");
            let (w, h) = (w + rng.range(-1, 1), h + rng.range(-1, 1));
            // Three rooms hang off the hub; the rest hang off those, so nothing crowds.
            let parent = if made <= 3 {
                "hub".to_string()
            } else {
                format!("r{}", (made - 4) % 3 + 1)
            };
            let kind = if made.is_multiple_of(4) {
                LinkKind::Opening
            } else {
                LinkKind::Door
            };
            rooms.push(room(&id, &name, w.clamp(3, 12), h.clamp(3, 10)));
            links.push(link(&parent, &id, kind));
        }
        if secret && fi == 0 {
            rooms.push(room("secret", set.secret, 5, 4));
            links.push(link("r1", "secret", LinkKind::Secret));
        }
        let mut stairs = Vec::new();
        if fi + 1 < floors_n {
            stairs.push(StairSpec {
                room: "hub".into(),
                to_floor: names[fi + 1].clone(),
                to_room: Some("entry".into()),
                label: None,
            });
        }
        if fi > 0 {
            stairs.push(StairSpec {
                room: "entry".into(),
                to_floor: names[fi - 1].clone(),
                to_room: Some("hub".into()),
                label: None,
            });
        }
        floors.push(FloorSpec {
            name: fname.clone(),
            rooms,
            links,
            stairs,
        });
    }
    MapSpec {
        name: title_of(&ws, set.title),
        floors,
    }
}

/// The smallest map that is always valid: used only if a seeded layout would not fit.
fn fallback() -> MapSpec {
    MapSpec {
        name: "Sketch Map".into(),
        floors: vec![FloorSpec {
            name: "Floor".into(),
            rooms: vec![
                room("entry", "Entrance Hall", 6, 5),
                room("hub", "Great Hall", 8, 6),
                room("r1", "Store Room", 5, 4),
            ],
            links: vec![
                link("entry", "hub", LinkKind::Opening),
                link("hub", "r1", LinkKind::Door),
            ],
            stairs: Vec::new(),
        }],
    }
}

/// A MapSpec from the words of `prompt` and `seed`; the same input always gives the same spec.
/// It validates and lays out (tries a few nearby seeds if one will not fit, then falls back
/// to a tiny map), and a "secret" in the prompt gives a secret link.
pub fn template_from_prompt(prompt: &str, seed: u64) -> MapSpec {
    for k in 0..6u64 {
        let spec = build(prompt, seed.wrapping_add(k * 7919));
        if spec.validate().is_ok() && compile(&spec, 1).is_ok() {
            return spec;
        }
    }
    fallback()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::speclint::lint_spec;

    const WORDS: &[&str] = &[
        "crypt",
        "tower",
        "ship",
        "station",
        "tavern",
        "mine",
        "cave",
        "dungeon",
        "castle",
        "secret",
        "hidden",
        "large",
        "small",
        "tiny",
        "huge",
        "3",
        "levels",
        "floors",
        "two",
        "floor",
        "kitchen",
        "library",
        "armory",
        "barracks",
        "shrine",
        "flooded",
        "abandoned",
        "metro",
        "the",
        "a",
        "with",
        "and",
        "99",
        "0",
        "decks",
        "wizard",
        "haunted",
        "zzz",
        "ünï",
        "!",
        "of",
        "dragon",
        "lair",
        "multi",
        "level",
        "vault",
        "prison",
        "cell",
    ];

    #[test]
    fn two_hundred_random_prompts_always_give_a_valid_laid_out_lint_clean_map() {
        let mut rng = Rng(42);
        for i in 0..200u64 {
            let n = 1 + rng.next() as usize % 7;
            let prompt: Vec<&str> = (0..n)
                .map(|_| WORDS[rng.next() as usize % WORDS.len()])
                .collect();
            let prompt = prompt.join(" ");
            let spec = template_from_prompt(&prompt, i * 31 + 5);
            spec.validate()
                .unwrap_or_else(|e| panic!("'{prompt}' did not validate: {e}"));
            let problems =
                lint_spec(&spec, 1).unwrap_or_else(|e| panic!("'{prompt}' did not lay out: {e}"));
            assert!(
                problems
                    .iter()
                    .all(|p| p.severity != crate::lint::Severity::Error),
                "'{prompt}': {problems:?}"
            );
            assert_eq!(
                spec,
                template_from_prompt(&prompt, i * 31 + 5),
                "deterministic"
            );
        }
        // Also the empty prompt.
        assert!(template_from_prompt("", 1).validate().is_ok());
    }

    #[test]
    fn levels_secrets_and_named_rooms_are_read_from_the_words() {
        let s = template_from_prompt("abandoned metro station, 3 levels, flooded", 1);
        assert_eq!(s.floors.len(), 3);
        // Stairs go both ways between neighbours.
        assert_eq!(s.floors[0].stairs.len(), 1);
        assert_eq!(s.floors[1].stairs.len(), 2);
        assert_eq!(s.floors[2].stairs.len(), 1);
        assert!(s.floors[1]
            .stairs
            .iter()
            .any(|st| st.to_floor == s.floors[0].name));
        assert_eq!(
            template_from_prompt("a tower of two floors", 1)
                .floors
                .len(),
            2
        );
        assert_eq!(
            template_from_prompt("99 levels", 1).floors.len(),
            MAX_TEMPLATE_FLOORS
        );
        assert_eq!(template_from_prompt("a crypt", 1).floors.len(), 1);

        let s = template_from_prompt("a crypt with a secret chamber", 1);
        let secret_links = s
            .floors
            .iter()
            .flat_map(|f| &f.links)
            .filter(|l| l.kind == LinkKind::Secret)
            .count();
        assert!(secret_links >= 1);
        let plain = template_from_prompt("a crypt", 1);
        assert!(plain
            .floors
            .iter()
            .flat_map(|f| &f.links)
            .all(|l| l.kind != LinkKind::Secret));

        let s = template_from_prompt("a tavern with a library", 1);
        assert!(s.floors[0].rooms.iter().any(|r| r.name == "Library"));
        // Another seed lays the same graph out differently but stays valid.
        assert!(template_from_prompt("a tavern", 2).validate().is_ok());
    }
}
