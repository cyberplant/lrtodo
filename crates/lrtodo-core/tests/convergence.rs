use std::collections::BTreeMap;

use lrtodo_core::*;
use proptest::prelude::*;

/// Servidor mínimo en memoria con las mismas reglas que el real.
#[derive(Clone)]
struct MemServer {
    clock: Clock,
    lists: BTreeMap<ListId, (ListMeta, ListDoc)>,
}

impl MemServer {
    fn new(lists: &[(&str, Caps, Vec<Task>)]) -> Self {
        Self {
            clock: Clock::new("server"),
            lists: lists
                .iter()
                .map(|(id, caps, tasks)| {
                    let meta = ListMeta {
                        id: id.to_string(),
                        name: id.to_string(),
                        kind: ListKind::Local,
                        caps: *caps,
                    };
                    (id.to_string(), (meta, ListDoc::from_tasks(tasks.clone())))
                })
                .collect(),
        }
    }

    fn sync(&mut self, req: SyncRequest, now_ms: u64) -> SyncResponse {
        if let Some(h) = &req.hlc {
            self.clock.observe(h);
        }
        let mut acked = vec![];
        let mut rejected = vec![];
        for op in &req.ops {
            self.clock.observe(&op.hlc);
            match self.lists.get_mut(&op.list) {
                Some((meta, doc)) => {
                    doc.apply_batch([op], &meta.caps, &mut acked, &mut rejected);
                }
                None => rejected.push(Rejection {
                    op: op.id.clone(),
                    list: op.list.clone(),
                    reason: "no existe".into(),
                }),
            }
        }
        let lists = self
            .lists
            .values()
            .map(|(meta, doc)| ListInfo {
                meta: meta.clone(),
                rev: doc.rev(),
                pending_upstream: 0,
                error: None,
                upstream_ms: None,
            })
            .collect::<Vec<_>>();
        let snapshots = lists
            .iter()
            .filter(|i| req.known.get(&i.meta.id) != Some(&i.rev))
            .map(|i| ListSnapshot {
                list: i.meta.id.clone(),
                rev: i.rev.clone(),
                tasks: self.lists[&i.meta.id].1.tasks.clone(),
            })
            .collect();
        SyncResponse {
            lists,
            snapshots,
            acked,
            rejected,
            hlc: self.clock.tick(now_ms),
        }
    }
}

fn seed_tasks() -> Vec<Task> {
    (0..4)
        .map(|i| Task::new(format!("t{i}"), format!("tarea {i}")))
        .collect()
}

#[derive(Clone, Debug)]
enum Action {
    Toggle(usize),
    Rename(usize, u8),
    Tag(usize, u8),
    Delete(usize),
    Add(u8),
    Sync,
}

fn action() -> impl Strategy<Value = Action> {
    prop_oneof![
        3 => (0..8usize).prop_map(Action::Toggle),
        2 => (0..8usize, any::<u8>()).prop_map(|(i, n)| Action::Rename(i, n)),
        1 => (0..8usize, any::<u8>()).prop_map(|(i, n)| Action::Tag(i, n)),
        1 => (0..8usize).prop_map(Action::Delete),
        2 => any::<u8>().prop_map(Action::Add),
        2 => Just(Action::Sync),
    ]
}

fn run(replica: &mut Replica, server: &mut MemServer, action: &Action, now: u64) {
    let tasks = replica.view("compras").unwrap();
    let pick = |i: usize| tasks.get(i % tasks.len().max(1)).map(|t| t.task.clone());
    match action {
        Action::Toggle(i) => {
            if let Some(t) = pick(*i) {
                replica
                    .set("compras", &t.id, Change::Done(!t.done), now)
                    .unwrap();
            }
        }
        Action::Rename(i, n) => {
            if let Some(t) = pick(*i) {
                replica
                    .set("compras", &t.id, Change::Title(format!("x{n}")), now)
                    .unwrap();
            }
        }
        Action::Tag(i, n) => {
            if let Some(t) = pick(*i) {
                replica
                    .set("compras", &t.id, Change::Tags(vec![format!("tag{n}")]), now)
                    .unwrap();
            }
        }
        Action::Delete(i) => {
            if let Some(t) = pick(*i) {
                replica.delete("compras", &t.id, now).unwrap();
            }
        }
        Action::Add(n) => {
            replica
                .add("compras", Task::new("", format!("nueva {n}")), now)
                .unwrap();
        }
        Action::Sync => {
            let resp = server.sync(replica.sync_request(), now);
            replica.apply_response(resp, now);
        }
    }
}

fn sorted(mut tasks: Vec<Task>) -> Vec<Task> {
    tasks.sort_by(|a, b| a.id.cmp(&b.id));
    tasks
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(300))]

    /// Varios dispositivos editan offline y sincronizan en cualquier orden:
    /// todos terminan viendo lo mismo que el servidor.
    #[test]
    fn replicas_converge(
        script in prop::collection::vec((0..3usize, action(), 0..5u64), 1..60),
    ) {
        let mut server = MemServer::new(&[("compras", Caps::ALL, seed_tasks())]);
        let mut replicas: Vec<Replica> = (0..3)
            .map(|i| Replica::new(format!("dev{i}"), i as u64 + 1))
            .collect();
        for r in &mut replicas {
            let resp = server.sync(r.sync_request(), 1000);
            r.apply_response(resp, 1000);
        }
        // Relojes desfasados por dispositivo, a propósito.
        let mut now = [2000u64, 1500, 2500];
        for (who, action, dt) in &script {
            now[*who] += dt;
            run(&mut replicas[*who], &mut server, action, now[*who]);
        }
        // Dos rondas de sync: la primera vacía outboxes, la segunda trae lo último.
        for _ in 0..2 {
            for (i, r) in replicas.iter_mut().enumerate() {
                let resp = server.sync(r.sync_request(), now[i] + 10_000);
                r.apply_response(resp, now[i] + 10_000);
            }
        }
        let truth = server.lists["compras"].1.tasks.clone();
        for r in &replicas {
            prop_assert_eq!(r.pending(), 0);
            let view: Vec<Task> = r.view("compras").unwrap().into_iter().map(|t| t.task).collect();
            prop_assert_eq!(&view, &truth);
        }
    }

    /// El resultado en el servidor no depende del orden en que llegan los
    /// lotes de distintos dispositivos (sólo del contenido).
    #[test]
    fn server_order_independent(
        scripts in prop::collection::vec(prop::collection::vec((action(), 0..5u64), 0..15), 3),
        order_seed in any::<u64>(),
    ) {
        let base = MemServer::new(&[("compras", Caps::ALL, seed_tasks())]);
        // Cada dispositivo trabaja offline sobre el estado inicial.
        let mut batches = vec![];
        for (i, script) in scripts.iter().enumerate() {
            let mut r = Replica::new(format!("dev{i}"), i as u64 + 7);
            let mut scratch = base.clone();
            let resp = scratch.sync(r.sync_request(), 1000);
            r.apply_response(resp, 1000);
            let mut now = 2000 + i as u64 * 333;
            for (a, dt) in script {
                now += dt;
                if !matches!(a, Action::Sync) {
                    run(&mut r, &mut scratch, a, now);
                }
            }
            batches.push(r.sync_request());
        }
        let deliver = |order: &[usize]| {
            let mut s = base.clone();
            for &i in order {
                s.sync(batches[i].clone(), 50_000);
            }
            sorted(s.lists["compras"].1.tasks.clone())
        };
        let mut order: Vec<usize> = (0..batches.len()).collect();
        let a = deliver(&order);
        let mut rng = lrtodo_core::Rng::new(order_seed);
        for i in (1..order.len()).rev() {
            order.swap(i, (rng.next_u64() % (i as u64 + 1)) as usize);
        }
        let b = deliver(&order);
        prop_assert_eq!(a, b);
    }
}

#[test]
fn offline_view_and_rebase() {
    let mut server = MemServer::new(&[("compras", Caps::ALL, seed_tasks())]);
    let mut phone = Replica::new("phone", 1);
    let mut laptop = Replica::new("laptop", 2);
    for r in [&mut phone, &mut laptop] {
        let resp = server.sync(r.sync_request(), 1000);
        r.apply_response(resp, 1000);
    }
    // En el súper sin señal: el teléfono tilda cosas y agrega otra.
    phone
        .set("compras", "t0", Change::Done(true), 2000)
        .unwrap();
    let pan = phone.add("compras", Task::new("", "pan"), 2001).unwrap();
    let view = phone.view("compras").unwrap();
    assert!(
        view.iter()
            .any(|t| t.task.id == "t0" && t.task.done && t.pending)
    );
    assert!(view.iter().any(|t| t.task.id == pan && t.pending));
    assert_eq!(phone.pending(), 2);
    phone.sync_failed("timeout");
    assert_eq!(phone.view("compras").unwrap().len(), 5);

    // Mientras tanto la laptop renombra t1 y borra t2.
    laptop
        .set("compras", "t1", Change::Title("leche".into()), 2500)
        .unwrap();
    laptop.delete("compras", "t2", 2600).unwrap();
    let resp = server.sync(laptop.sync_request(), 2700);
    laptop.apply_response(resp, 2700);

    // El teléfono recupera señal.
    let resp = server.sync(phone.sync_request(), 3000);
    phone.apply_response(resp, 3000);
    assert_eq!(phone.pending(), 0);
    assert!(phone.last_error.is_none());
    let view = phone.view("compras").unwrap();
    let titles: Vec<_> = view.iter().map(|t| t.task.title.as_str()).collect();
    assert_eq!(titles, ["tarea 0", "leche", "tarea 3", "pan"]);
    assert!(view[0].task.done && view[0].task.completed.is_some());
}

#[test]
fn unsupported_fields_rejected() {
    let mut server = MemServer::new(&[("ha:super", Caps::HOME_ASSISTANT, seed_tasks())]);
    let mut r = Replica::new("p", 1);
    let resp = server.sync(r.sync_request(), 1000);
    r.apply_response(resp, 1000);
    assert!(
        r.set("ha:super", "t0", Change::Tags(vec!["x".into()]), 2000)
            .is_err()
    );
    let mut t = Task::new("", "pan");
    t.tags = vec!["a".into()];
    let id = r.add("ha:super", t, 2000).unwrap();
    let resp = server.sync(r.sync_request(), 2001);
    r.apply_response(resp, 2001);
    let view = r.view("ha:super").unwrap();
    assert!(
        view.iter()
            .any(|t| t.task.id == id && t.task.tags.is_empty())
    );
}

#[test]
fn ingest_external_edits() {
    let mut doc = ListDoc::default();
    let mut clock = Clock::new("srv");
    let mut n = 0;
    let mut new_id = || {
        n += 1;
        format!("gen{n}")
    };
    assert!(doc.ingest(
        vec![Task::new("", "a"), Task::new("b", "b")],
        &mut clock,
        1000,
        &mut new_id
    ));
    assert_eq!(doc.tasks[0].id, "gen1");
    assert!(!doc.ingest(doc.tasks.clone(), &mut clock, 1001, &mut new_id));

    // Edición a mano: se renombra b, se borra a.
    let mut edited = doc.tasks[1].clone();
    edited.title = "B".into();
    assert!(doc.ingest(vec![edited], &mut clock, 2000, &mut new_id));
    assert_eq!(doc.tasks.len(), 1);
    assert!(doc.tombstones.contains_key("gen1"));

    // Una op vieja (offline desde antes) pierde contra la edición a mano.
    let stale = Op {
        id: "op".into(),
        list: "l".into(),
        hlc: Hlc::new(1500, 0, "phone"),
        kind: OpKind::Set {
            id: "b".into(),
            change: Change::Title("viejo".into()),
        },
    };
    assert_eq!(doc.apply(&stale, &Caps::ALL), Outcome::Ignored);
    assert_eq!(doc.tasks[0].title, "B");
}
