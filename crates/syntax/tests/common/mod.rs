//! Shared by the integration tests: one small source sample per language, and helpers to
//! ask what a piece of a sample was highlighted as.

#![allow(dead_code)]

use bakedit_syntax::{language_by_id, HighlightSpan, Highlighter, Language};

/// A language id and a short, valid source file in that language.
pub struct Sample {
    pub language: &'static str,
    pub text: &'static str,
}

pub const SAMPLES: &[Sample] = &[
    Sample {
        language: "rust",
        text: RUST,
    },
    Sample {
        language: "python",
        text: PYTHON,
    },
    Sample {
        language: "javascript",
        text: JAVASCRIPT,
    },
    Sample {
        language: "typescript",
        text: TYPESCRIPT,
    },
    Sample {
        language: "tsx",
        text: TSX,
    },
    Sample {
        language: "json",
        text: JSON,
    },
    Sample {
        language: "toml",
        text: TOML,
    },
    Sample {
        language: "yaml",
        text: YAML,
    },
    Sample {
        language: "markdown",
        text: MARKDOWN,
    },
    Sample {
        language: "shell",
        text: SHELL,
    },
    Sample {
        language: "go",
        text: GO,
    },
    Sample {
        language: "c",
        text: C,
    },
    Sample {
        language: "cpp",
        text: CPP,
    },
    Sample {
        language: "html",
        text: HTML,
    },
    Sample {
        language: "css",
        text: CSS,
    },
    Sample {
        language: "swift",
        text: SWIFT,
    },
    Sample {
        language: "java",
        text: JAVA,
    },
    Sample {
        language: "lua",
        text: LUA,
    },
    Sample {
        language: "dockerfile",
        text: DOCKERFILE,
    },
    Sample {
        language: "sql",
        text: SQL,
    },
    Sample {
        language: "ruby",
        text: RUBY,
    },
    Sample {
        language: "kotlin",
        text: KOTLIN,
    },
    Sample {
        language: "diff",
        text: DIFF,
    },
    Sample {
        language: "make",
        text: MAKE,
    },
    Sample {
        language: "nix",
        text: NIX,
    },
    Sample {
        language: "zig",
        text: ZIG,
    },
    Sample {
        language: "php",
        text: PHP,
    },
    Sample {
        language: "csharp",
        text: CSHARP,
    },
    Sample {
        language: "dart",
        text: DART,
    },
    Sample {
        language: "gomod",
        text: GOMOD,
    },
    Sample {
        language: "git_commit",
        text: GIT_COMMIT,
    },
];

pub const RUST: &str = r##"//! Module docs.
use std::collections::HashMap;

/// A point.
#[derive(Debug, Clone)]
pub struct Point<T> {
    x: T,
    label: &'static str,
}

const LIMIT: usize = 10;

impl<T: Copy> Point<T> {
    // Build one.
    pub fn new(x: T) -> Self {
        let greeting = "hi\n";
        let on = true;
        let total = 1.5 + 2 as f64;
        println!("{} {}", greeting, total);
        let items = vec![Some(1), None];
        Point { x, label: "p" }.shifted(items.len())
    }

    fn shifted(self, by: usize) -> Self {
        if by > LIMIT { self } else { helper(self) }
    }
}

fn helper<T>(value: T) -> T {
    /* block */
    value
}
"##;

pub const PYTHON: &str = r##"#!/usr/bin/env python3
"""Module docstring."""
import os
from typing import List

LIMIT = 10


class Shape:
    sides: int = 0

    def __init__(self, name: str) -> None:
        self.name = name  # trailing comment

    @staticmethod
    def area(width, height=2.5):
        label = f"area {width * height}\n"
        print(label)
        return len(label) if width else None


def main(argv: List[str]) -> bool:
    shape = Shape("square")
    for index in range(LIMIT):
        os.path.join("a", str(index))
    return True
"##;

pub const JAVASCRIPT: &str = r##"// A comment.
import { readFile } from "node:fs";

const LIMIT = 10;
const pattern = /ab+c/gi;

class Shape extends Base {
  constructor(name) {
    super();
    this.name = name;
  }

  area(width, height = 2.5) {
    const label = `area ${width * height}`;
    console.log(label);
    return width ? label.length : null;
  }
}

export function main(argv) {
  const shape = new Shape("square");
  const double = (x) => x * 2;
  return shape.area(double(LIMIT), true) !== undefined;
}

const view = <div className="app">{main()}</div>;
"##;

pub const TYPESCRIPT: &str = r##"// A comment.
import type { Reader } from "./reader";

export interface Shape {
  readonly name: string;
  area(width: number): number;
}

type Id = string | number;

enum Color {
  Red = 1,
  Green,
}

export abstract class Base<T> implements Shape {
  private count = 0;
  constructor(public readonly name: string) {}

  area(width: number, scale?: number): number {
    const label: string = `area ${width}`;
    console.log(label, scale);
    return width * 2.5;
  }
}

export function main(argv: string[]): boolean {
  const ids: Id[] = [1, "two"];
  return ids.length > 0 && true;
}
"##;

pub const TSX: &str = r##"import React from "react";

interface Props {
  title: string;
  count?: number;
}

export function Card({ title, count = 0 }: Props): JSX.Element {
  const label: string = `${title}: ${count}`;
  // A comment.
  return (
    <section className="card" onClick={() => console.log(label)}>
      <h1>{label}</h1>
      <Badge value={count} />
    </section>
  );
}
"##;

pub const JSON: &str = r##"{
  // A comment, as in a settings file.
  "name": "bakedit",
  "version": 1.5,
  "private": true,
  "license": null,
  "tags": ["editor", "terminal\n"],
  "nested": { "depth": 2 }
}
"##;

pub const TOML: &str = r##"# A comment.
name = "bakedit"
version = 1
ratio = 1.5
enabled = true
released = 2026-11-22

[package]
edition = "2021"
authors = ["a", 'b']
target.dir = "out"

[[bin]]
path = "src/main.rs"

[dependencies]
serde = { version = "1", features = ["derive"] }
"##;

pub const YAML: &str = r##"# A comment.
apiVersion: v1
kind: ConfigMap
metadata:
  name: "bakedit"
  labels: &labels
    app: editor
spec:
  replicas: 3
  ratio: 1.5
  enabled: true
  empty: null
  items:
    - first
    - 'second'
  copy: *labels
  script: |
    echo hi
"##;

pub const MARKDOWN: &str = r##"---
title: Notes
---

# Bakedit *notes*

Some **bold** and *italic* text with `inline code`, a [link](https://example.com "tip")
and an <https://auto.link>.

## `Code` first

- item one
- item \* two

> A quote.

```rust
fn main() {
    let total = 1 + 2;
}
```

```
plain fence
```

    indented code

Setext
======

<div class="note">raw html</div>
"##;

pub const SHELL: &str = r##"#!/usr/bin/env bash
# A comment.
set -euo pipefail

NAME="world"
readonly COUNT=3

greet() {
  local who="$1"
  echo "hello ${who}" 'literal' >&2
}

for i in $(seq 1 "$COUNT"); do
  if [[ -n "$NAME" ]]; then
    greet "$NAME" | tr a-z A-Z
  fi
done

case "$NAME" in
  world) echo ok ;;
  *) exit 1 ;;
esac

cat <<END
heredoc $NAME
END
"##;

pub const GO: &str = r##"// Package main is a sample.
package main

import (
	"fmt"
	"strings"
)

const Limit = 10

type Point struct {
	X, Y int
	Name string
}

func (p Point) Area() float64 {
	return float64(p.X*p.Y) * 1.5
}

func main() {
	point := Point{X: 1, Y: 2, Name: "p\n"}
	items := make([]string, 0, Limit)
	if len(items) == 0 && true {
		fmt.Println(strings.ToUpper(point.Name), point.Area(), nil)
	}
	/* block */
}
"##;

pub const C: &str = r##"// A comment.
#include <stdio.h>
#include "local.h"

#define LIMIT 10
#define SQUARE(x) ((x) * (x))

typedef struct point {
    int x;
    const char *name;
} point_t;

static int add(int a, int b) {
    return a + b;
}

int main(int argc, char **argv) {
    point_t p = { .x = 1, .name = "p\n" };
    char c = '(';
    if (argc > LIMIT && true) {
        printf("%d %s\n", add(p.x, 2), p.name);
    }
    /* block */
    return 0x1F;
}
"##;

pub const CPP: &str = r##"// A comment.
#include <string>
#include <vector>

namespace geo {

template <typename T>
class Shape {
public:
    explicit Shape(std::string name) : name_(std::move(name)) {}
    virtual ~Shape() = default;
    virtual double area() const = 0;

private:
    std::string name_;
};

constexpr int kLimit = 10;

auto twice(int value) -> int {
    return value * 2;
}

}  // namespace geo

int main() {
    std::vector<int> items{1, 2, 3};
    auto raw = R"(raw text)";
    bool on = true;
    if (items.size() > 0 && on) {
        return geo::twice(geo::kLimit);
    }
    return nullptr == nullptr ? 0 : 1;
}
"##;

pub const HTML: &str = r##"<!DOCTYPE html>
<html lang="en">
  <head>
    <meta charset="utf-8">
    <title>Sample</title>
    <style>
      body { color: #333; margin: 0 auto; }
    </style>
  </head>
  <body class="page">
    <!-- A comment. -->
    <h1 id="top">Hello</h1>
    <script>
      const greeting = "hi";
      function show() { return greeting.length; }
    </script>
  </body>
</html>
"##;

pub const CSS: &str = r##"/* A comment. */
@import "base.css";

:root {
  --accent: #82aaff;
}

body, .card > h1 {
  color: var(--accent);
  margin: 0 auto;
  font: 14px/1.5 "Fira Code", monospace;
}

#main:hover::before {
  content: "x";
  width: calc(100% - 2em) !important;
}

@media (min-width: 600px) {
  a[href^="http"] { opacity: 0.5; }
}
"##;

pub const SWIFT: &str = r##"// A comment.
import Foundation

protocol Shape {
    var sides: Int { get }
    func area() -> Double
}

struct Point: Shape {
    let sides = 0
    var name: String = "p\n"

    init(name: String) {
        self.name = name
    }

    func area() -> Double {
        let scale = 2.5
        print("area \(scale)")
        return Double(sides) * scale
    }
}

enum Color {
    case red, green
}

/// Doc comment.
func main(count: Int) -> Bool {
    let point = Point(name: "square")
    guard count > 0 else { return false }
    for index in 0..<count where index != 3 {
        _ = point.area()
    }
    return true
}
"##;

pub const JAVA: &str = r##"// A comment.
package sample;

import java.util.List;

public class Shape implements Comparable<Shape> {
    private static final int LIMIT = 10;
    private final String name;

    public Shape(String name) {
        this.name = name;
    }

    @Override
    public int compareTo(Shape other) {
        return name.compareTo(other.name);
    }

    /** Doc comment. */
    public static void main(String[] args) {
        List<Shape> shapes = List.of(new Shape("square\n"));
        double ratio = 1.5;
        if (shapes.size() > LIMIT && true) {
            System.out.println(shapes.get(0).name + ratio + 'c' + null);
        }
    }
}
"##;

pub const LUA: &str = r##"-- A comment.
local M = {}
local LIMIT = 10

--[[ block
comment ]]
function M.area(width, height)
  local label = "area\n" .. tostring(width * height)
  print(label)
  return #label > 0 and true or nil
end

local function helper(self, n)
  for i = 1, n do
    if i % 2 == 0 then
      M.area(i, 2.5)
    end
  end
  return self
end

function M:method()
  return helper(self, LIMIT)
end

return M
"##;

pub const DOCKERFILE: &str = r##"# A comment.
ARG VERSION=1.75
FROM rust:${VERSION} AS build
WORKDIR /app
COPY . .
ENV RUSTFLAGS="-C target-cpu=native" MODE=release
RUN cargo build --release \
    && strip target/release/bakedit
EXPOSE 8080
ENTRYPOINT ["/app/bakedit", "--serve"]
"##;

pub const SQL: &str = r##"-- A comment.
CREATE TABLE users (
    id INTEGER PRIMARY KEY,
    name VARCHAR(40) NOT NULL,
    active BOOLEAN DEFAULT TRUE
);

/* block */
SELECT u.name, COUNT(*) AS total
FROM users AS u
JOIN orders o ON o.user_id = u.id
WHERE u.active = TRUE AND o.amount > 10.5 AND u.name LIKE 'a%'
GROUP BY u.name
ORDER BY total DESC
LIMIT 5;
"##;

pub const RUBY: &str = r##"# A comment.
require "json"

module Geometry
  LIMIT = 10

  class Shape
    attr_reader :name

    def initialize(name)
      @name = name
    end

    def area(width, height = 2.5)
      label = "area #{width * height}\n"
      puts label
      width.zero? ? nil : label.length
    end

    def self.build
      new("square")
    end
  end
end

shape = Geometry::Shape.build
[1, 2].each { |n| shape.area(n) if n > 0 && true }
"##;

pub const KOTLIN: &str = r##"// A comment.
package sample

import kotlin.math.max

const val LIMIT = 10

data class Point(val x: Int, val name: String = "p\n")

interface Shape {
    fun area(): Double
}

class Circle(private val radius: Double) : Shape {
    override fun area(): Double {
        val scale = 2.5
        println("area ${radius * scale}")
        return max(radius, scale)
    }
}

/* block */
fun main(args: Array<String>) {
    val shape: Shape = Circle(1.0)
    if (args.size > LIMIT && true) {
        shape.area()
    }
    val nothing = null
}
"##;

pub const DIFF: &str = r##"diff --git a/src/main.rs b/src/main.rs
index 83db48f..bf2a1c3 100644
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,4 +1,4 @@
 fn main() {
-    println!("old");
+    println!("new");
 }
"##;

pub const MAKE: &str = r##"# A comment.
CC := clang
CFLAGS += -O2 -Wall
OBJECTS = $(patsubst %.c,%.o,$(wildcard src/*.c))

.PHONY: all clean

all: bakedit

bakedit: $(OBJECTS)
	$(CC) $(CFLAGS) -o $@ $^

%.o: %.c
	$(CC) $(CFLAGS) -c $< -o $@

ifeq ($(DEBUG),1)
CFLAGS += -g
endif

clean:
	rm -f $(OBJECTS) bakedit

include extra.mk
"##;

pub const NIX: &str = r##"# A comment.
{ pkgs ? import <nixpkgs> { }, lib, ... }:

let
  version = "1.0";
  limit = 10;
  double = x: x * 2;
in
pkgs.stdenv.mkDerivation rec {
  pname = "bakedit";
  inherit version;
  src = ./.;
  buildInputs = with pkgs; [ rustc cargo ];
  enabled = true;
  ratio = 1.5;
  buildPhase = ''
    cargo build --release
    echo ${toString (double limit)}
  '';
  meta.description = "An editor\n";
  /* block */
}
"##;

pub const ZIG: &str = r##"//! Module docs.
const std = @import("std");

// A comment.
const LIMIT: usize = 10;

const Point = struct {
    x: i32,
    name: []const u8 = "p\n",

    pub fn area(self: Point, scale: f64) f64 {
        return @as(f64, @floatFromInt(self.x)) * scale;
    }
};

pub fn main() !void {
    var point = Point{ .x = 1 };
    const ratio = 2.5;
    if (point.x > 0 and true) {
        std.debug.print("area {d}\n", .{point.area(ratio)});
    }
    for (0..LIMIT) |index| {
        _ = index;
    }
    return error.Failed;
}
"##;

pub const PHP: &str = r##"<h1>Title</h1>
<?php
// A comment.
namespace App\Geometry;

use App\Base\Shape;

const LIMIT = 10;

final class Circle extends Shape implements \Countable
{
    private float $radius = 2.5;

    public function __construct(string $name)
    {
        $this->name = $name;
    }

    public function area(int $width): ?float
    {
        $label = "area {$width}\n";
        echo strlen($label), 'literal';
        return $width > LIMIT && true ? $this->radius * $width : null;
    }
}

/* block */
function main(array $argv): bool
{
    $shape = new Circle("round");
    return $shape->area(count($argv)) !== null;
}
?>
<p class="footer">done</p>
"##;

pub const CSHARP: &str = r##"// A comment.
using System;
using System.Collections.Generic;

namespace Sample
{
    public interface IShape
    {
        double Area();
    }

    public class Circle : IShape
    {
        private const int Limit = 10;
        public string Name { get; set; } = "c\n";

        public Circle(string name)
        {
            Name = name;
        }

        public double Area()
        {
            var scale = 2.5;
            Console.WriteLine($"area {scale}");
            return scale * Limit;
        }

        /* block */
        public static void Main(string[] args)
        {
            IShape shape = new Circle("round");
            if (args.Length > Limit && true)
            {
                shape.Area();
            }
            object nothing = null;
        }
    }
}
"##;

pub const DART: &str = r##"// A comment.
import 'dart:math';

const limit = 10;

abstract class Shape {
  double area();
}

class Circle implements Shape {
  final double radius;
  String name = "c\n";

  Circle(this.radius);

  @override
  double area() {
    final scale = 2.5;
    print('area ${radius * scale}');
    return max(radius, scale);
  }
}

/// Doc comment.
void main(List<String> args) {
  final Shape shape = Circle(1.0);
  if (args.length > limit && true) {
    shape.area();
  }
  var nothing = null;
}
"##;

pub const GOMOD: &str = r##"// A comment.
module example.com/bakedit

go 1.23

require (
	github.com/example/dep v1.2.3
	golang.org/x/sys v0.20.0 // indirect
)

replace example.com/old => example.com/new v1.0.0
"##;

pub const GIT_COMMIT: &str = r##"feat(syntax): add the highlighter

A longer body that explains what changed and why it
changed.

BREAKING CHANGE: spans are byte offsets
Signed-off-by: Some One <some.one@example.com>

# Please enter the commit message for your changes. Lines starting
# with '#' will be ignored, and an empty message aborts the commit.
#
# On branch feat/syntax
# Changes to be committed:
#	modified:   crates/syntax/src/lib.rs
#	new file:   crates/syntax/src/highlight.rs
#
"##;

/// The language with this id, which must exist.
pub fn language(id: &str) -> &'static Language {
    language_by_id(id).unwrap_or_else(|| panic!("no language {id}"))
}

/// Highlight `text` as the language with this id.
pub fn highlight(id: &str, text: &str) -> Vec<HighlightSpan> {
    Highlighter::new()
        .highlight(language(id), text, None)
        .unwrap_or_else(|error| panic!("highlighting {id} failed: {error}"))
}

/// The byte offset of the `n`th (from 0) occurrence of `needle` in `text`.
pub fn nth_offset(text: &str, needle: &str, n: usize) -> usize {
    text.match_indices(needle)
        .nth(n)
        .map(|(offset, _)| offset)
        .unwrap_or_else(|| panic!("occurrence {n} of {needle:?} is not in the text"))
}

/// The capture every byte of `start..end` is drawn with: its name, or `""` when no span
/// covers any of it. Panics when the range is drawn with more than one capture or only
/// partly highlighted, because then "the capture of" it has no answer.
pub fn capture_of_range(
    text: &str,
    spans: &[HighlightSpan],
    start: usize,
    end: usize,
) -> &'static str {
    let mut found: Option<&'static str> = None;
    let mut position = start;
    for span in spans
        .iter()
        .filter(|span| span.end > start && span.start < end)
    {
        let name = span.capture.name();
        assert!(
            span.start <= position && found.is_none_or(|previous| previous == name),
            "{:?} is not drawn with one capture:\n{}",
            &text[start..end],
            render(text, spans)
        );
        found = Some(name);
        position = span.end;
    }
    match found {
        Some(name) if position >= end => name,
        Some(_) => panic!(
            "{:?} is only partly highlighted:\n{}",
            &text[start..end],
            render(text, spans)
        ),
        None => "",
    }
}

/// The capture the `n`th occurrence of `needle` is drawn with; see [`capture_of_range`].
pub fn capture_of_nth(text: &str, spans: &[HighlightSpan], needle: &str, n: usize) -> &'static str {
    let start = nth_offset(text, needle, n);
    capture_of_range(text, spans, start, start + needle.len())
}

/// The capture the first occurrence of `needle` is drawn with.
pub fn capture_of(text: &str, spans: &[HighlightSpan], needle: &str) -> &'static str {
    capture_of_nth(text, spans, needle, 0)
}

/// The capture of a piece of text picked out by its surroundings. `marked` is text that
/// occurs in `text`, with the piece in question between `«` and `»`: `"fn «main»("` asks
/// about the `main` that follows the first `fn `. Without the marks it asks about all of
/// `marked`. See [`capture_of_range`] for the answer.
pub fn capture_marked(text: &str, spans: &[HighlightSpan], marked: &str) -> &'static str {
    let (context, inner) = match (marked.find('«'), marked.find('»')) {
        (Some(open), Some(close)) if open < close => {
            let before = &marked[..open];
            let piece = &marked[open + '«'.len_utf8()..close];
            let after = &marked[close + '»'.len_utf8()..];
            (
                format!("{before}{piece}{after}"),
                before.len()..before.len() + piece.len(),
            )
        }
        _ => (marked.to_owned(), 0..marked.len()),
    };
    let start = nth_offset(text, &context, 0);
    capture_of_range(text, spans, start + inner.start, start + inner.end)
}

/// Every span as `start..end capture "text"`, one per line, for reading in a failure
/// message.
pub fn render(text: &str, spans: &[HighlightSpan]) -> String {
    spans
        .iter()
        .map(|span| {
            let name = span.capture.name();
            format!(
                "{:>5}..{:<5} {name:<34} {:?}\n",
                span.start,
                span.end,
                &text[span.range()]
            )
        })
        .collect()
}

/// A small deterministic random number generator (xorshift64*), so the tests that mangle
/// samples do the same thing on every run.
pub struct Random(u64);

impl Random {
    pub fn new(seed: u64) -> Random {
        Random(seed.max(1))
    }

    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// A number in `0..bound`, or 0 when `bound` is 0.
    pub fn below(&mut self, bound: usize) -> usize {
        if bound == 0 {
            0
        } else {
            (self.next() % bound as u64) as usize
        }
    }
}
