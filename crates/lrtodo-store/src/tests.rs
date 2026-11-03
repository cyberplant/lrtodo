use std::sync::Arc;

use lrtodo_core::{Change, Hlc, Op, OpKind, Task};

use super::*;

fn add_op(list: &str, id: &str, title: &str, ms: u64) -> Op {
    Op {
        id: format!("op-{id}"),
        list: list.into(),
        hlc: Hlc::new(ms, 0, "test"),
        kind: OpKind::Add {
            task: Task::new(id, title),
        },
    }
}

fn titles(store: &Store, id: &str) -> Vec<String> {
    store
        .read(id)
        .unwrap()
        .doc
        .tasks
        .iter()
        .map(|t| t.title.clone())
        .collect()
}

#[test]
fn file_layout_roundtrip_and_manual_edit() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path(), "t").unwrap();
    store
        .create_list("compras", Some("Compras"), Layout::File)
        .unwrap();
    store
        .mutate("compras", |l| {
            l.doc
                .apply(&add_op("compras", "A1", "leche", now_ms()), &Caps::ALL);
        })
        .unwrap();
    let text = fs::read_to_string(dir.path().join("compras.yaml")).unwrap();
    assert!(text.contains("name: Compras"), "{text}");
    assert!(text.contains("title: leche"), "{text}");

    // Edición a mano: agregar un item sin id, tildar otro y un campo propio.
    let edited = text.replace(
        "title: leche",
        "title: leche\n  done: yes\n  marca: Conaprole",
    ) + "- title: pan\n";
    fs::write(dir.path().join("compras.yaml"), edited).unwrap();
    let loaded = store.read("compras").unwrap();
    assert_eq!(loaded.doc.tasks.len(), 2);
    assert!(loaded.doc.tasks[0].done);
    assert_eq!(loaded.doc.tasks[0].extra["marca"], "Conaprole");
    assert!(!loaded.doc.tasks[1].id.is_empty());
    // El archivo quedó normalizado con ids.
    let text = fs::read_to_string(dir.path().join("compras.yaml")).unwrap();
    assert!(
        text.contains(&format!("id: {}", loaded.doc.tasks[1].id)),
        "{text}"
    );
    assert!(text.contains("marca: Conaprole"), "{text}");
}

#[test]
fn dir_layout_and_convert() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path(), "t").unwrap();
    store.create_list("casa", None, Layout::Dir).unwrap();
    store
        .mutate("casa", |l| {
            l.doc
                .apply(&add_op("casa", "B1", "pintar", 1000), &Caps::ALL);
            l.doc
                .apply(&add_op("casa", "B2", "regar", 2000), &Caps::ALL);
        })
        .unwrap();
    // Los archivos son NNNN-slug.yaml (posición + slug del título).
    assert!(dir.path().join("casa/0001-pintar.yaml").is_file());
    assert_eq!(titles(&store, "casa"), ["pintar", "regar"]);

    // Borrar un archivo a mano = borrar la tarea.
    fs::remove_file(dir.path().join("casa/0001-pintar.yaml")).unwrap();
    assert_eq!(titles(&store, "casa"), ["regar"]);
    // Un archivo nuevo a mano, sin id: toma el nombre del archivo; el
    // save lo renombra a NNNN-slug (regar pasa a posición 1).
    fs::write(dir.path().join("casa/barrer.yaml"), "title: barrer\n").unwrap();
    assert_eq!(titles(&store, "casa").len(), 2);
    assert!(dir.path().join("casa/0002-barrer.yaml").is_file());

    store.convert("casa", Layout::File).unwrap();
    assert!(!dir.path().join("casa").exists());
    assert_eq!(store.layout("casa").unwrap(), Layout::File);
    assert_eq!(titles(&store, "casa").len(), 2);
    store.convert("casa", Layout::Dir).unwrap();
    assert!(dir.path().join("casa/0002-barrer.yaml").is_file());
    assert_eq!(store.list_ids().unwrap(), ["casa"]);
}

#[test]
fn stale_op_loses_against_manual_edit() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path(), "t").unwrap();
    store.create_list("l", None, Layout::File).unwrap();
    store
        .mutate("l", |l| {
            l.doc.apply(&add_op("l", "C1", "a", 1000), &Caps::ALL)
        })
        .unwrap();
    let path = dir.path().join("l.yaml");
    let text = fs::read_to_string(&path)
        .unwrap()
        .replace("title: a", "title: b");
    fs::write(&path, text).unwrap();
    let (loaded, _) = store
        .mutate("l", |l| {
            let op = Op {
                id: "x".into(),
                list: "l".into(),
                hlc: Hlc::new(1500, 0, "phone"),
                kind: OpKind::Set {
                    id: "C1".into(),
                    change: Change::Title("viejo".into()),
                },
            };
            l.doc.apply(&op, &Caps::ALL)
        })
        .unwrap();
    assert_eq!(loaded.doc.tasks[0].title, "b");
}

#[test]
fn concurrent_writers_do_not_lose_updates() {
    for layout in [Layout::File, Layout::Dir] {
        let dir = tempfile::tempdir().unwrap();
        let root = Arc::new(dir.path().to_path_buf());
        Store::open(&*root, "init")
            .unwrap()
            .create_list("l", None, layout)
            .unwrap();
        let threads: Vec<_> = (0..8)
            .map(|t| {
                let root = root.clone();
                std::thread::spawn(move || {
                    // Cada hilo con su propio Store = como procesos distintos.
                    let store = Store::open(&*root, &format!("w{t}")).unwrap();
                    for i in 0..10 {
                        let id = format!("T{t}x{i}");
                        store
                            .mutate("l", |l| {
                                l.doc.apply(&add_op("l", &id, &id, now_ms()), &Caps::ALL)
                            })
                            .unwrap();
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        let store = Store::open(&*root, "check").unwrap();
        assert_eq!(store.read("l").unwrap().doc.tasks.len(), 80, "{layout:?}");
        assert!(store.check().is_empty());
    }
}

#[test]
fn remove_list_deletes_data_and_state() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path(), "t").unwrap();
    for (id, layout) in [("a", Layout::File), ("b", Layout::Dir)] {
        store.create_list(id, None, layout).unwrap();
        store
            .mutate(id, |l| {
                l.doc.apply(&add_op(id, "T1", "x", 1000), &Caps::ALL)
            })
            .unwrap();
    }
    assert_eq!(store.list_ids().unwrap(), ["a", "b"]);
    store.remove_list("a").unwrap();
    store.remove_list("b").unwrap();
    assert_eq!(store.list_ids().unwrap(), Vec::<String>::new());
    assert!(!dir.path().join("a.yaml").exists());
    assert!(!dir.path().join("b").exists());
    // El estado de sync también se fue.
    assert!(
        fs::read_dir(dir.path().join(".lrtodo/state"))
            .unwrap()
            .next()
            .is_none()
    );
    // Y se puede recrear limpio.
    store.create_list("a", None, Layout::File).unwrap();
    assert!(matches!(
        store.remove_list("nada"),
        Err(StoreError::NotFound(_))
    ));
}

#[test]
fn check_reports_problems() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("mala.yaml"),
        "items:\n<<<<<<< HEAD\n- title: x\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("dup.yaml"),
        "items:\n- {id: A, title: x}\n- {id: A, title: y}\n",
    )
    .unwrap();
    let store = Store::open(dir.path(), "t").unwrap();
    let problems = store.check();
    assert_eq!(problems.len(), 2, "{problems:?}");
    assert!(matches!(store.read("mala"), Err(StoreError::Parse { .. })));
    assert!(matches!(store.read("nada"), Err(StoreError::NotFound(_))));
    assert!(matches!(store.read("../x"), Err(StoreError::InvalidId(_))));
}

#[test]
fn add_asset_achica_imagen() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path(), "test").unwrap();
    store.create_list("c", None, Layout::Dir).unwrap();
    let (loaded, _) = store
        .mutate("c", |l| {
            let op = add_op("c", "t1", "vender mac", 1);
            l.doc.apply(&op, &l.meta.caps)
        })
        .unwrap();
    let id = loaded.doc.tasks[0].id.clone();

    // PNG 2000x1200 sin alpha → debería salir jpeg ≤1600px.
    let img = image::RgbImage::new(2000, 1200);
    let mut png = Vec::new();
    use image::ImageEncoder as _;
    image::codecs::png::PngEncoder::new(&mut png)
        .write_image(img.as_raw(), 2000, 1200, image::ExtendedColorType::Rgb8)
        .unwrap();
    let rel = store.add_asset("c", &id, "foto grande.png", &png).unwrap();
    assert!(rel.ends_with(".jpg"), "rel: {rel}");
    let out = image::load_from_memory(&std::fs::read(dir.path().join(&rel)).unwrap()).unwrap();
    assert!(out.width() <= 1600 && out.height() <= 1600);
    assert_eq!(out.width(), 1600); // 2000→1600, 1200→960

    // Texto plano pasa tal cual.
    let rel2 = store.add_asset("c", &id, "nota.txt", b"hola").unwrap();
    assert!(
        rel2.starts_with("c/") && rel2.ends_with(".assets/nota.txt"),
        "rel2: {rel2}"
    );
    assert_eq!(std::fs::read(dir.path().join(&rel2)).unwrap(), b"hola");
}
