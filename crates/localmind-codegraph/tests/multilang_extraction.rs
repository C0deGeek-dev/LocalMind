//! Per-language node-extraction fixtures: a small idiomatic source per language
//! must yield the expected definition/symbol nodes through the native provider.

use localmind_codegraph::{AdmittedFile, CodeIntelligenceProvider, NativeProvider};
use localmind_core::{GraphNode, NodeKind};
use std::path::PathBuf;

fn extract(relative: &str, source: &str) -> Vec<GraphNode> {
    let file = AdmittedFile {
        absolute: PathBuf::from("unused"),
        relative: relative.to_string(),
    };
    let mut provider = match NativeProvider::new() {
        Ok(provider) => provider,
        Err(error) => unreachable!("provider must build: {error}"),
    };
    match provider.parse_file(&file, source) {
        Ok(parsed) => parsed.items,
        Err(error) => unreachable!("{relative} must parse: {error}"),
    }
}

fn has(items: &[GraphNode], kind: NodeKind, name: &str) -> bool {
    items
        .iter()
        .any(|item| item.kind == kind && item.name == name)
}

fn assert_has(items: &[GraphNode], kind: NodeKind, name: &str) {
    assert!(
        has(items, kind, name),
        "expected a {kind:?} named {name:?}; got {:?}",
        items
            .iter()
            .map(|item| (item.kind, item.name.as_str()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn python_definitions() {
    let items = extract(
        "app/main.py",
        "class Animal:\n    def speak(self):\n        return noise()\n\ndef greet():\n    return 1\n",
    );
    assert_has(&items, NodeKind::Type, "Animal");
    assert_has(&items, NodeKind::Function, "greet");
    assert_has(&items, NodeKind::Function, "speak");
}

#[test]
fn go_definitions() {
    let items = extract(
        "pkg/geo.go",
        "package geo\n\ntype Point struct { X int }\n\nfunc Add(a, b int) int { return a + b }\n",
    );
    assert_has(&items, NodeKind::Type, "Point");
    assert_has(&items, NodeKind::Function, "Add");
}

#[test]
fn javascript_definitions() {
    let items = extract(
        "src/app.js",
        "export function run() { return 1; }\n\nclass Widget { render() {} }\n",
    );
    assert_has(&items, NodeKind::Function, "run");
    assert_has(&items, NodeKind::Type, "Widget");
}

#[test]
fn typescript_definitions() {
    let items = extract(
        "src/api.ts",
        "export interface Shape { area(): number; }\n\nexport function run(): void {}\n\nclass Box {}\n",
    );
    assert_has(&items, NodeKind::Function, "run");
    assert_has(&items, NodeKind::Type, "Box");
    assert_has(&items, NodeKind::Type, "Shape");
}

#[test]
fn tsx_definitions() {
    let items = extract("ui/App.tsx", "export function App() { return null; }\n");
    assert_has(&items, NodeKind::Function, "App");
}

#[test]
fn csharp_definitions() {
    let items = extract(
        "Program.cs",
        "namespace N { class C { void F() { G(); } void G() {} } }\n",
    );
    assert_has(&items, NodeKind::Type, "C");
    assert_has(&items, NodeKind::Function, "F");
}

#[test]
fn java_definitions() {
    let items = extract(
        "Main.java",
        "class Main { void run() { help(); } void help() {} }\n",
    );
    assert_has(&items, NodeKind::Type, "Main");
    assert_has(&items, NodeKind::Function, "run");
}

#[test]
fn c_definitions() {
    let items = extract(
        "src/util.c",
        "int g(void) { return 0; }\nint f(void) { return g(); }\n",
    );
    assert_has(&items, NodeKind::Function, "f");
    assert_has(&items, NodeKind::Function, "g");
}

#[test]
fn cpp_definitions() {
    let items = extract(
        "src/util.cpp",
        "struct Vec { int x; };\nint add(int a, int b) { return a + b; }\n",
    );
    assert_has(&items, NodeKind::Function, "add");
    assert_has(&items, NodeKind::Type, "Vec");
}

#[test]
fn ruby_definitions() {
    let items = extract(
        "lib/thing.rb",
        "class Thing\n  def run\n    help\n  end\n  def help\n    1\n  end\nend\n",
    );
    assert_has(&items, NodeKind::Type, "Thing");
    assert_has(&items, NodeKind::Function, "run");
}

#[test]
fn php_definitions() {
    let items = extract(
        "src/app.php",
        "<?php\nclass Service { function run() { $this->help(); } function help() {} }\nfunction main() {}\n",
    );
    assert_has(&items, NodeKind::Type, "Service");
    assert_has(&items, NodeKind::Function, "main");
}

#[test]
fn lua_definitions() {
    let items = extract(
        "src/mod.lua",
        "function f()\n  return g()\nend\n\nfunction g()\n  return 1\nend\n",
    );
    assert_has(&items, NodeKind::Function, "f");
    assert_has(&items, NodeKind::Function, "g");
}

#[test]
fn ocaml_definitions() {
    let items = extract(
        "src/m.ml",
        "let g () = 1\nlet f () = g ()\nmodule M = struct let x = 1 end\n",
    );
    assert_has(&items, NodeKind::Function, "f");
}

#[test]
fn elixir_definitions() {
    let items = extract(
        "lib/m.ex",
        "defmodule M do\n  def run, do: help()\n  def help, do: 1\nend\n",
    );
    assert_has(&items, NodeKind::Function, "run");
}

#[test]
fn powershell_definitions() {
    let items = extract(
        "build.ps1",
        "function Get-Thing {\n  param($x)\n  Write-Output $x\n}\n\nGet-Thing 1\n",
    );
    assert_has(&items, NodeKind::Function, "Get-Thing");
}
