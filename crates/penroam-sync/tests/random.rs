//! Randomized convergence: three devices make random edits offline, sync in
//! random order with dropped responses and restarts, and must end identical.

mod common;

use common::*;
use penroam_core::*;
use penroam_sync_testkit::InMemoryServer;
use std::sync::Arc;
use uuid::Uuid;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
}

fn random_edit(c: &mut Client, rng: &mut Rng, tag: u64, counter: &mut u64) {
    let ids = c.lib.notebook_ids();
    if ids.is_empty() || rng.below(20) == 0 {
        c.lib
            .create_notebook(
                &format!("nb {tag}-{counter}"),
                PageDefaults::default(),
                None,
            )
            .unwrap();
        *counter += 1;
        return;
    }
    let nb = ids[rng.below(ids.len() as u64) as usize];
    let n = c.lib.notebook(nb).unwrap().clone();
    if n.deleted_at.is_some() {
        c.lib.restore_notebook(nb).unwrap();
        return;
    }
    let live = n.page_order.clone();
    *counter += 1;
    match rng.below(10) {
        0 => {
            c.lib
                .rename_notebook(nb, &format!("title {tag}-{counter}"))
                .unwrap();
        }
        1 => {
            c.lib
                .create_page(
                    nb,
                    Some(rng.below(live.len() as u64 + 1) as usize),
                    None,
                    None,
                )
                .unwrap();
        }
        2 if live.len() > 1 => {
            c.lib
                .delete_page(nb, live[rng.below(live.len() as u64) as usize])
                .unwrap();
        }
        3 if !n.trashed_pages.is_empty() => {
            c.lib.restore_page(nb, n.trashed_pages[0].id).unwrap();
        }
        4 if live.len() > 1 => {
            let mut order = live.clone();
            let i = rng.below(order.len() as u64) as usize;
            let p = order.remove(i);
            order.insert(rng.below(order.len() as u64 + 1) as usize, p);
            c.lib.update_page_order(nb, order).unwrap();
        }
        5 | 6 if !live.is_empty() => {
            let p = live[rng.below(live.len() as u64) as usize];
            let page = c.lib.load_page(nb, p).unwrap();
            if let Some(Element::Text(t)) =
                page.elements.iter().find(|e| matches!(e, Element::Text(_)))
            {
                let mut t = t.clone();
                t.text = format!("text {tag}-{counter}");
                edit(
                    &mut c.lib,
                    nb,
                    p,
                    OpKind::ReplaceElements {
                        elements: vec![Element::Text(t)],
                    },
                );
            } else {
                let id = (tag as u128) << 64 | *counter as u128;
                add(
                    &mut c.lib,
                    nb,
                    p,
                    vec![text(id, &format!("text {tag}-{counter}"))],
                );
            }
        }
        7 if !live.is_empty() => {
            let p = live[rng.below(live.len() as u64) as usize];
            let page = c.lib.load_page(nb, p).unwrap();
            if let Some(e) = page.elements.iter().find(|e| matches!(e, Element::Ink(_))) {
                edit(
                    &mut c.lib,
                    nb,
                    p,
                    OpKind::RemoveElements { ids: vec![e.id()] },
                );
            }
        }
        _ if !live.is_empty() => {
            let p = live[rng.below(live.len() as u64) as usize];
            let mut s = stroke(*counter, tag as f64 * 100.0, "#111111");
            if let Element::Ink(i) = &mut s {
                i.id = Uuid::from_u128((tag as u128) << 80 | *counter as u128);
            }
            add(&mut c.lib, nb, p, vec![s]);
        }
        _ => {}
    }
}

fn run(seed: u64) {
    let s = Arc::new(InMemoryServer::new());
    let acct = account(&s, &format!("rand-{seed}"));
    let (mut a, _) = Client::first(&s, &acct, "a");
    a.lib
        .create_notebook("start", PageDefaults::default(), None)
        .unwrap();
    a.sync().unwrap();
    let b = Client::paired(&s, &acct, "b", &mut a);
    let c = Client::paired(&s, &acct, "c", &mut a);
    let mut clients = vec![a, b, c];
    for c in clients.iter_mut() {
        c.sync().unwrap();
    }
    let mut rng = Rng(seed * 2654435761 + 1);
    let mut counter = 0u64;
    for _round in 0..12 {
        for _ in 0..rng.below(12) {
            let i = rng.below(3) as usize;
            random_edit(&mut clients[i], &mut rng, i as u64 + 1, &mut counter);
        }
        // random syncs, some with a lost response or a crash
        for _ in 0..3 {
            let i = rng.below(3) as usize;
            match rng.below(8) {
                0 => s.drop_next_responses(1),
                1 => s.fail_next_requests(1, penroam_sync_protocol::ErrorCode::ServiceUnavailable),
                _ => {}
            }
            let _ = clients[i].sync();
            if rng.below(10) == 0 {
                let c = clients.remove(i);
                clients.insert(i, c.restart());
            }
        }
    }
    s.drop_next_responses(0);
    for _ in 0..4 {
        for c in clients.iter_mut() {
            c.sync().unwrap();
        }
    }
    let v0 = view(&mut clients[0].lib);
    for c in clients.iter_mut().skip(1) {
        let v = view(&mut c.lib);
        if v != v0 {
            let mut out = Vec::new();
            diff("", &v0, &v, &mut out);
            panic!("seed {seed}: devices diverged:\n{}", out.join("\n"));
        }
    }
    for c in &clients {
        let st = c.status();
        assert!(st.up_to_date, "seed {seed}: {st:?}");
        assert_eq!(st.quarantined, 0);
    }
}

fn diff(path: &str, a: &serde_json::Value, b: &serde_json::Value, out: &mut Vec<String>) {
    use serde_json::Value::*;
    if out.len() > 40 {
        return;
    }
    match (a, b) {
        (Object(x), Object(y)) => {
            for k in x.keys().chain(y.keys().filter(|k| !x.contains_key(*k))) {
                let n = serde_json::Value::Null;
                diff(
                    &format!("{path}.{k}"),
                    x.get(k).unwrap_or(&n),
                    y.get(k).unwrap_or(&n),
                    out,
                );
            }
        }
        (Array(x), Array(y)) if x.len() == y.len() => {
            for (i, (p, q)) in x.iter().zip(y).enumerate() {
                diff(&format!("{path}[{i}]"), p, q, out);
            }
        }
        _ if a != b => {
            let s = |v: &serde_json::Value| {
                let t = v.to_string();
                if t.len() > 300 {
                    format!("{}…", &t[..300])
                } else {
                    t
                }
            };
            out.push(format!("{path}: {} != {}", s(a), s(b)));
        }
        _ => {}
    }
}

#[test]
fn random_concurrent_edits_converge() {
    for seed in 1..=12 {
        run(seed);
    }
}
