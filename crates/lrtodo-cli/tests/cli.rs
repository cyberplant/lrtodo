use std::path::Path;
use std::process::Command;

use assert_cmd::cargo::CommandCargoExt;

fn lrtodo(data: &Path) -> Command {
    let mut cmd = Command::cargo_bin("lrtodo").unwrap();
    cmd.env("LRTODO_CONFIG", "/dev/null")
        .env_remove("LRTODO_SERVER")
        .env("NO_COLOR", "1")
        .arg("--data-dir")
        .arg(data);
    cmd
}

fn run(data: &Path, args: &[&str]) -> String {
    let out = lrtodo(data).args(args).output().unwrap();
    assert!(
        out.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn basic_flow() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    run(d, &["new", "compras", "--name", "Compras"]);
    run(d, &["add", "compras", "leche", "-t", "super"]);
    run(d, &["add", "compras", "pan"]);
    run(d, &["done", "compras", "1"]);
    let ls = run(d, &["ls", "compras"]);
    assert!(ls.contains("1 [x] leche #super"), "{ls}");
    assert!(ls.contains("2 [ ] pan"), "{ls}");
    run(
        d,
        &[
            "set",
            "compras",
            "pan",
            "--title",
            "pan integral",
            "--due",
            "2026-11-01",
        ],
    );
    run(d, &["rm", "compras", "leche"]);
    let ls = run(d, &["ls", "compras"]);
    assert!(ls.contains("1 [ ] pan integral (vence 2026-11-01)"), "{ls}");
    assert!(
        !lrtodo(d)
            .args(["done", "compras", "99"])
            .output()
            .unwrap()
            .status
            .success()
    );
}

#[test]
fn edit_with_editor_merges() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    run(d, &["new", "l"]);
    run(d, &["add", "l", "a"]);
    run(d, &["add", "l", "b"]);
    // "Editor" que renombra a, borra b y agrega c.
    let script = d.join("ed.sh");
    std::fs::write(
        &script,
        r#"#!/usr/bin/env python3
import os, re, subprocess, sys
# Mientras se edita, otro proceso agrega una tarea: no se tiene que perder.
subprocess.run([os.environ["LRTODO_BIN"], "--data-dir", os.environ["LRTODO_TEST_DIR"], "add", "l", "z"], check=True)
p = sys.argv[1]
s = open(p).read()
head, _, rest = s.partition("items:\n")
blocks = [b for b in re.split(r"(?m)^(?=- )", rest) if "title: b\n" not in b]
open(p, "w").write(head + "items:\n" + "".join(blocks).replace("title: a\n", "title: A\n") + "- title: c\n")
"#,
    )
    .unwrap();
    Command::new("chmod")
        .arg("+x")
        .arg(&script)
        .status()
        .unwrap();
    let out = lrtodo(d)
        .args(["edit", "l"])
        .env("EDITOR", &script)
        .env_remove("VISUAL")
        .env("LRTODO_BIN", assert_cmd::cargo::cargo_bin("lrtodo"))
        .env("LRTODO_TEST_DIR", d)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let ls = run(d, &["ls", "l"]);
    assert!(ls.contains("[ ] A"), "{ls}");
    assert!(ls.contains("[ ] c"), "{ls}");
    assert!(ls.contains("[ ] z"), "{ls}");
    assert!(!ls.contains("[ ] b"), "{ls}");
}

#[test]
fn concurrent_processes() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    for layout in ["file", "dir"] {
        run(d, &["new", layout, "--layout", layout]);
        let children: Vec<_> = (0..12)
            .map(|i| {
                lrtodo(d)
                    .args(["add", layout, &format!("tarea {i}")])
                    .spawn()
                    .unwrap()
            })
            .collect();
        for mut c in children {
            assert!(c.wait().unwrap().success());
        }
        let ls = run(d, &["ls", layout]);
        assert!(ls.contains("(12 abiertas de 12)"), "{layout}: {ls}");
    }
    assert_eq!(run(d, &["check"]).trim(), "ok");
}
