use std::sync::{Arc, Mutex};
use std::time::Duration;

use lrtodo_core::{Change, Hlc, Op, OpKind, Task};
use lrtodo_ha::{HaClient, HaItem, HaList, features};
use serde_json::{Value, json};
use wiremock::matchers::{method, path_regex};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const ENTITY: &str = "todo.super";

/// Home Assistant falso con estado, que entiende los servicios `todo.*`.
#[derive(Clone, Default)]
struct FakeHa {
    items: Arc<Mutex<Vec<HaItem>>>,
    next: Arc<Mutex<u32>>,
    down: Arc<Mutex<bool>>,
}

impl FakeHa {
    fn item(&self, summary: &str) -> HaItem {
        let mut n = self.next.lock().unwrap();
        *n += 1;
        HaItem {
            uid: format!("uid-{n}"),
            summary: summary.into(),
            status: "needs_action".into(),
            due: None,
            description: None,
        }
    }
    fn add(&self, summary: &str) -> String {
        let item = self.item(summary);
        let uid = item.uid.clone();
        self.items.lock().unwrap().push(item);
        uid
    }
    fn summaries(&self) -> Vec<(String, String)> {
        self.items
            .lock()
            .unwrap()
            .iter()
            .map(|i| (i.summary.clone(), i.status.clone()))
            .collect()
    }
}

impl Respond for FakeHa {
    fn respond(&self, req: &Request) -> ResponseTemplate {
        if *self.down.lock().unwrap() {
            return ResponseTemplate::new(502);
        }
        let path = req.url.path();
        if path == "/api/states" {
            return ResponseTemplate::new(200).set_body_json(json!([
                {"entity_id": ENTITY, "state": "1", "attributes": {"friendly_name": "Súper", "supported_features": 127}},
                {"entity_id": "light.x", "state": "on", "attributes": {}},
            ]));
        }
        let body: Value = serde_json::from_slice(&req.body).unwrap();
        let service = path.rsplit('/').next().unwrap();
        let mut items = self.items.lock().unwrap();
        match service {
            "get_items" => {
                return ResponseTemplate::new(200).set_body_json(json!({
                    "changed_states": [],
                    "service_response": {ENTITY: {"items": *items}},
                }));
            }
            "add_item" => {
                drop(items);
                let mut item = self.item(body["item"].as_str().unwrap());
                item.due = body
                    .get("due_date")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                item.description = body
                    .get("description")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                self.items.lock().unwrap().push(item);
            }
            "update_item" => {
                let key = body["item"].as_str().unwrap();
                let Some(item) = items.iter_mut().find(|i| i.uid == key || i.summary == key) else {
                    return ResponseTemplate::new(400);
                };
                if let Some(r) = body.get("rename").and_then(Value::as_str) {
                    item.summary = r.into();
                }
                if let Some(s) = body.get("status").and_then(Value::as_str) {
                    item.status = s.into();
                }
                if let Some(d) = body.get("due_date") {
                    item.due = d.as_str().map(str::to_string);
                }
                if let Some(d) = body.get("description") {
                    item.description = d.as_str().map(str::to_string);
                }
            }
            "remove_item" => {
                let uids: Vec<String> = serde_json::from_value(body["item"].clone()).unwrap();
                items.retain(|i| !uids.contains(&i.uid));
            }
            _ => return ResponseTemplate::new(404),
        }
        ResponseTemplate::new(200).set_body_json(json!([]))
    }
}

async fn setup() -> (MockServer, FakeHa, HaClient) {
    let server = MockServer::start().await;
    let fake = FakeHa::default();
    Mock::given(method("GET"))
        .and(path_regex("^/api/states$"))
        .respond_with(fake.clone())
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path_regex("^/api/services/todo/.*"))
        .respond_with(fake.clone())
        .mount(&server)
        .await;
    let client = HaClient::new(&server.uri(), "token", Duration::from_secs(2)).unwrap();
    (server, fake, client)
}

fn op(id: &str, ms: u64, kind: OpKind) -> Op {
    Op {
        id: id.into(),
        list: "ha:super".into(),
        hlc: Hlc::new(ms, 0, "phone"),
        kind,
    }
}

fn titles(list: &HaList) -> Vec<(String, bool)> {
    list.doc
        .tasks
        .iter()
        .map(|t| (t.title.clone(), t.done))
        .collect()
}

#[tokio::test]
async fn discovers_entities() {
    let (_s, _fake, client) = setup().await;
    let entities = client.todo_entities().await.unwrap();
    assert_eq!(entities.len(), 1);
    assert_eq!(entities[0].name, "Súper");
    assert_eq!(entities[0].features, 127);
}

#[tokio::test]
async fn two_way_sync() {
    let (_s, fake, client) = setup().await;
    fake.add("leche");
    let pan = fake.add("pan");
    let state = tokio::sync::Mutex::new(HaList::new(ENTITY, "Súper", 127, 1));
    let now = || 1_000_000u64;

    // Lee lo que hay en HA.
    assert!(HaList::reconcile(&state, &client, &now).await.unwrap());
    assert_eq!(
        titles(&*state.lock().await),
        [("leche".into(), false), ("pan".into(), false)]
    );

    // Desde lrtodo: tildar pan, agregar huevos (con nota), borrar leche.
    {
        let mut s = state.lock().await;
        let leche = s.doc.tasks[0].id.clone();
        let mut huevos = Task::new("H1", "huevos");
        huevos.notes = Some("docena".into());
        let ops = [
            op(
                "o1",
                2_000_000,
                OpKind::Set {
                    id: pan.clone(),
                    change: Change::Done(true),
                },
            ),
            op("o2", 2_000_001, OpKind::Add { task: huevos }),
            op("o3", 2_000_002, OpKind::Delete { id: leche }),
        ];
        let (mut acked, mut rejected) = (vec![], vec![]);
        assert!(s.apply_ops(&ops, &mut acked, &mut rejected));
        assert_eq!(acked.len(), 3);
        assert!(s.pending_upstream() > 0);
    }
    HaList::reconcile(&state, &client, &now).await.unwrap();
    assert_eq!(
        fake.summaries(),
        [
            ("pan".into(), "completed".into()),
            ("huevos".into(), "needs_action".into())
        ]
    );
    {
        let s = state.lock().await;
        assert_eq!(s.pending_upstream(), 0);
        // El id local de huevos se mantiene, mapeado al uid de HA.
        assert_eq!(s.doc.tasks[1].id, "H1");
        assert_eq!(s.doc.tasks[1].notes.as_deref(), Some("docena"));
    }

    // Cambios hechos en HA (app, voz): renombrar huevos, agregar café, borrar pan.
    {
        let mut items = fake.items.lock().unwrap();
        items.retain(|i| i.summary != "pan");
        items
            .iter_mut()
            .for_each(|i| i.summary = "huevos x12".into());
    }
    fake.add("café");
    assert!(HaList::reconcile(&state, &client, &now).await.unwrap());
    let s = state.lock().await;
    assert_eq!(
        titles(&s),
        [("huevos x12".into(), false), ("café".into(), false)]
    );
    assert_eq!(s.doc.tasks[0].id, "H1");
    assert_eq!(s.pending_upstream(), 0);
    assert!(s.error.is_none());
}

#[tokio::test]
async fn ha_down_keeps_changes_pending() {
    let (_s, fake, client) = setup().await;
    let uid = fake.add("leche");
    let state = tokio::sync::Mutex::new(HaList::new(ENTITY, "Súper", 127, 1));
    let now = || 1_000_000u64;
    HaList::reconcile(&state, &client, &now).await.unwrap();

    *fake.down.lock().unwrap() = true;
    {
        let mut s = state.lock().await;
        let ops = [op(
            "o1",
            2_000_000,
            OpKind::Set {
                id: uid.clone(),
                change: Change::Done(true),
            },
        )];
        s.apply_ops(&ops, &mut vec![], &mut vec![]);
    }
    assert!(HaList::reconcile(&state, &client, &now).await.is_err());
    {
        let s = state.lock().await;
        assert!(s.error.is_some());
        assert_eq!(s.pending_upstream(), 1);
        // Los clientes siguen viendo el estado deseado.
        assert!(s.doc.tasks[0].done);
    }

    *fake.down.lock().unwrap() = false;
    HaList::reconcile(&state, &client, &now).await.unwrap();
    assert_eq!(fake.summaries(), [("leche".into(), "completed".into())]);
    let s = state.lock().await;
    assert!(s.error.is_none());
    assert_eq!(s.pending_upstream(), 0);
}

#[tokio::test]
async fn add_is_not_duplicated_when_refetch_fails() {
    let (_s, fake, client) = setup().await;
    let state = tokio::sync::Mutex::new(HaList::new(ENTITY, "Súper", 127, 1));
    let now = || 1_000_000u64;
    HaList::reconcile(&state, &client, &now).await.unwrap();
    {
        let mut s = state.lock().await;
        let ops = [op(
            "o1",
            2_000_000,
            OpKind::Add {
                task: Task::new("P1", "pan"),
            },
        )];
        s.apply_ops(&ops, &mut vec![], &mut vec![]);
        // Simula que el add llegó a HA pero se perdió la respuesta.
        let actions = s.plan_push();
        assert_eq!(actions.len(), 1);
        s.record_pushed(actions[0].clone(), now());
    }
    fake.add("pan");
    HaList::reconcile(&state, &client, &now).await.unwrap();
    assert_eq!(fake.summaries().len(), 1);
    let s = state.lock().await;
    assert_eq!(s.doc.tasks.len(), 1);
    assert_eq!(s.doc.tasks[0].id, "P1");
}

#[tokio::test]
async fn unsupported_operations_rejected() {
    // Lista de compras básica de HA: sin borrar ni fechas.
    let mut list = HaList::new(ENTITY, "Súper", features::CREATE | features::UPDATE, 1);
    assert!(!list.caps().due);
    let mut rejected = vec![];
    list.apply_ops(
        &[op("o1", 1, OpKind::Delete { id: "x".into() })],
        &mut vec![],
        &mut rejected,
    );
    assert_eq!(rejected.len(), 1);
}
