//! What each language's tokens are highlighted as.
//!
//! One table per language: a piece of the shared sample and the capture it must be drawn
//! with. A piece is named by text that occurs in the sample, with `«»` around the part in
//! question when its surroundings are needed to find it. `""` means no highlight at all.

mod common;

use common::{check_captures, highlight, SAMPLES};

/// Check the language's sample against a table of `(piece, capture)`.
fn check(language: &str, expectations: &[(&str, &str)]) {
    let sample = SAMPLES
        .iter()
        .find(|sample| sample.language == language)
        .unwrap_or_else(|| panic!("no sample for {language}"));
    check_captures(language, sample.text, expectations);
}

#[test]
fn rust() {
    check(
        "rust",
        &[
            ("//! Module docs.", "comment.documentation"),
            ("/// A point.", "comment.documentation"),
            ("// Build one.", "comment"),
            ("/* block */", "comment"),
            ("use", "keyword"),
            ("pub", "keyword"),
            ("struct", "keyword"),
            ("impl", "keyword"),
            ("«fn» new", "keyword"),
            ("«let» greeting", "keyword"),
            ("«if» by", "keyword"),
            ("2 «as» f64", "keyword"),
            ("struct «Point»", "type"),
            ("x: «T»,", "type"),
            ("«usize» = 10", "type.builtin"),
            ("«label»: &", "property"),
            ("'«static»", "lifetime"),
            ("const «LIMIT»", "constant"),
            ("by > «LIMIT»", "constant"),
            ("= «10»;", "number"),
            ("1.5", "number"),
            ("true", "boolean"),
            ("fn «new»", "function"),
            ("fn «shifted»", "function"),
            ("fn «helper»", "function"),
            ("«helper»(self)", "function"),
            (".«shifted»(", "function.method"),
            ("items.«len»()", "function.method"),
            ("println!", "function.macro"),
            ("vec!", "function.macro"),
            ("(«x»: T)", "variable.parameter"),
            ("«self», by", "variable.builtin"),
            ("\"hi", "string"),
            ("\"hi«\\n»\"", "string.escape"),
            ("\"{} {}\"", "string"),
            ("\"p\"", "string"),
            ("«#»[derive", "punctuation.special"),
            ("#[«derive»", "attribute"),
            ("total «=» 1.5", "operator"),
            ("1.5 «+» 2", "operator"),
            ("«->» Self", "operator"),
            ("by «>» LIMIT", "operator"),
            ("Point«<»T> {", "punctuation.bracket.open"),
            ("Point<T«>» {", "punctuation.bracket.close"),
            ("new«(»x", "punctuation.bracket"),
            ("collections«::»HashMap", "punctuation.delimiter"),
            ("«greeting» =", ""),
        ],
    );
}

#[test]
fn rust_macro_arguments_are_highlighted_as_rust() {
    // The arguments of a macro are an opaque token tree to the grammar; the bundled
    // injection re-parses them as Rust.
    check(
        "rust",
        &[
            ("vec![«Some»(1)", "type"),
            ("Some(«1»)", "number"),
            ("«None»]", "type"),
        ],
    );
}

#[test]
fn c_and_cpp_follow_zed_where_the_bundled_queries_do_not() {
    check_captures(
        "c",
        "#ifdef _WIN32\nint run(struct io *self) { goto out; out: return self->read(self) % 2 ? 'a' : '\\n'; }\n#endif\n",
        &[
            ("#ifdef «_WIN32»", "constant"),
            ("goto", "keyword"),
            // In C a function pointer called through a field is a property.
            ("self->«read»(", "property"),
            ("«%» 2", "operator"),
            ("2 «?» 'a'", "operator"),
            ("'a' «:» '", "operator"),
            ("'a'", "string"),
            ("'«\\n»'", "string.escape"),
        ],
    );
    check_captures(
        "cpp",
        "struct Point { int x; ~Point(); bool operator==(const Point&) const; };\n\
         int f(Point* p, Point q) { ASSERT_OK(p); return p->size() + q.size() + a::b::c(); }\n\
         #if defined(DEBUG)\n#endif\nclass D : public Point { public: int y; };\n",
        &[
            // In C++ a method is a function, through `.`, `->` or nested namespaces.
            ("p->«size»()", "function"),
            ("q.«size»()", "function"),
            ("a::b::«c»()", "function.call"),
            ("~«Point»()", "function"),
            ("«operator»==", "keyword"),
            ("operator«==»", "operator"),
            // A name in capitals is a built-in constant even where it is called.
            ("«ASSERT_OK»(p)", "constant.builtin"),
            ("defined(«DEBUG»)", "constant.macro"),
            ("«defined»(DEBUG)", "keyword.preproc"),
            ("q«.»size", "operator"),
            ("a«::»b", "punctuation.delimiter"),
            ("D «:» public", "punctuation.delimiter"),
            ("public«:» int y", "punctuation.delimiter"),
            ("&) «const»;", "keyword"),
        ],
    );
}

#[test]
fn the_javascript_family_follows_zed_where_the_bundled_queries_do_not() {
    let text = r#"import React, { useState } from "react";
import type { Store } from "./store";

const MAX_ITEMS = 3;
const escaped = "tab\there";

export class List<T> extends React.Component implements Store {
  #items: T[] = [];
  constructor(private readonly size: number) { super(); }
  static of(...values: unknown[]): List<unknown> | null {
    return values.length > MAX_ITEMS ? null : new List(values.length);
  }
}

export const Menu = ({ label, onPick }: { label: string; onPick: () => void }) => {
  const [open] = useState(false);
  Promise.resolve(label).then(onPick);
  return (
    <Menu.Item title={label} kind="plain">
      {open && <Fragment key={MAX_ITEMS} />}
    </Menu.Item>
  );
};
"#;
    check_captures(
        "tsx",
        text,
        &[
            // A capital alone does not make a type: only where a name stands does.
            ("import «React»", "variable"),
            ("extends «React».", "variable"),
            ("React.«Component»", "property"),
            ("new «List»(", "type.class"),
            ("class «List»", "type"),
            ("implements «Store»", "type"),
            ("«Promise».resolve", "type"),
            ("«MAX_ITEMS» = 3", "constant"),
            ("key={«MAX_ITEMS»}", "constant"),
            // A component is a function where it is defined and a tag where it is used.
            ("const «Menu» =", "function"),
            ("<«Menu».Item title", "tag.component.jsx"),
            ("<Menu.«Item» title", "tag.component.jsx"),
            ("</Menu.«Item»>", "tag.component.jsx"),
            ("<«Fragment» key", "tag.component.jsx"),
            ("«title»={label}", "attribute.jsx"),
            ("title«=»{label}", "punctuation.delimiter.jsx"),
            // Parameters, also destructured ones.
            ("({ «label», onPick }", "variable.parameter"),
            ("private readonly «size»", "variable.parameter"),
            ("...«values»:", "variable.parameter"),
            ("«useState»(false)", "function"),
            // Properties, the constructor, and the odds and ends.
            ("«#items»:", "property"),
            ("«constructor»(private", "constructor"),
            ("\"tab«\\t»here\"", "string.escape"),
            ("«...»values", "operator"),
            ("MAX_ITEMS «?» null", "operator"),
            ("null «:» new", "operator"),
            ("List«<»T>", "punctuation.bracket"),
            ("List<unknown> «|» null", "punctuation.special"),
        ],
    );
    // TypeScript proper: literal types and type-only imports.
    check_captures(
        "typescript",
        "import { type Reader, read } from \"./io\";\nlet done: true | null | undefined = null;\n",
        &[
            ("type «Reader»", "type"),
            ("«read» }", "variable"),
            ("done: «true»", "type.builtin"),
            ("| «null» |", "type.builtin"),
            ("| «undefined» =", "type.builtin"),
            ("= «null»;", "constant.builtin"),
        ],
    );
    // JavaScript proper, where a class's name is an ordinary identifier.
    check_captures(
        "javascript",
        "class Shape extends Base {}\nfunction area({ width, height = 2 }, _) { return width * height; }\n",
        &[
            ("class «Shape»", "type.class"),
            ("extends «Base»", "type.class"),
            ("{ «width»,", "variable.parameter"),
            ("«height» = 2", "variable.parameter"),
            ("}, «_»)", "constant"),
            // The bundled locals do not follow a destructured parameter into the body.
            ("return «width»", "variable"),
        ],
    );
}

#[test]
fn python_follows_zed_where_the_bundled_query_does_not() {
    let text = r#"import abc
import functools

_MAX_SIZE = 3


class _Base(abc.ABC, metaclass=abc.ABCMeta):
    @functools.wraps(print)
    def run(self, *args, **kwargs):
        if args and not kwargs or self is None:
            raise ValueError(type(self).__name__)
        return sorted(args, key=len, reverse=True)

    @property
    def size(self) -> "int | None":
        return _MAX_SIZE if isinstance(self, _Base) else ...
"#;
    check_captures(
        "python",
        text,
        &[
            // Names by convention, with or without leading underscores, attributes too.
            ("«_MAX_SIZE» = 3", "constant"),
            ("class «_Base»", "type.class"),
            ("abc.«ABC»,", "constant"),
            ("abc.«ABCMeta»", "type.class"),
            ("raise «ValueError»(", "type.class"),
            // Keyword arguments, here and in a class's arguments.
            ("«key»=len", "function.kwargs"),
            ("«reverse»=True", "function.kwargs"),
            ("«metaclass»=", "variable"),
            // Word operators are keywords.
            ("args «and» not", "keyword.operator"),
            ("and «not» kwargs", "keyword.operator"),
            ("kwargs «or» self", "keyword.operator"),
            ("self «is» None", "keyword.operator"),
            // Of a decorator the `@` and the names are function-coloured, the rest is not.
            ("«@»functools", "function.decorator"),
            ("@«functools».", "function.decorator"),
            ("@functools«.»wraps", "punctuation.delimiter"),
            (".«wraps»(print)", "function.decorator"),
            ("wraps«(»print", "punctuation.bracket"),
            ("«@»property", "attribute.builtin"),
            ("@«property»", "attribute.builtin"),
            // Parameters, dunders, builtins, annotations.
            ("*«args»,", "variable.parameter"),
            ("**«kwargs»)", "variable.parameter"),
            (").«__name__»", "attribute.special"),
            ("«type»(self)", "function.builtin"),
            ("«sorted»(args", "function.builtin"),
            ("«isinstance»(self", "function.builtin"),
            ("isinstance(self, «_Base»)", "type"),
            ("-> «\"int | None\"»", "type"),
            ("else «...»", "constant.builtin"),
        ],
    );
}

#[test]
fn rust_follows_zed_where_the_bundled_query_does_not() {
    let text = r#"#![allow(clippy::needless_return)]
#[cfg(not(test))]
#[derive(Debug)]
enum Shape { Dot, WIDE(u8) }

macro_rules! twice {
    ($value:expr) => { $value * 2 };
}

fn pick<'a>(first: &'a str, Limit: usize) -> Result<Shape, &'a str> {
    log::info!("picking {}", first);
    let Wrapper { inner, .. } = wrap(first);
    if first.len() > Limit { return Err(inner); }
    Ok(Shape::WIDE(twice!(4)))
}
"#;
    check_captures(
        "rust",
        text,
        &[
            // Only an attribute's name and its plain top-level arguments are attributes.
            ("«#»!", "punctuation.special"),
            ("#«!»[", "none"),
            ("«allow»(", "attribute"),
            ("«clippy»::", "none"),
            ("::«needless_return»", "attribute"),
            ("«cfg»(", "attribute"),
            ("(«not»(", "attribute"),
            ("not(«test»)", "none"),
            ("derive(«Debug»)", "type"),
            // A variant is a type where it is defined, whatever its capitals.
            ("{ «Dot»,", "type"),
            ("«WIDE»(u8)", "type"),
            // Macros: the definition, what it matches, and a call through a path.
            ("macro_rules! «twice»", "function.macro"),
            ("$value:«expr»", "type"),
            ("log::«info»", "function.macro"),
            ("info«!»", "function.macro"),
            ("«twice»!(4)", "function.macro"),
            // A lifetime is one thing, and not an operator.
            ("pick<«'»a>", "lifetime"),
            ("pick<'«a»>", "lifetime"),
            ("&«'»a str, L", "lifetime"),
            // A parameter is a parameter even with a capital; elsewhere a capital is a type,
            // also when it is called.
            ("«Limit»: usize", "variable.parameter"),
            ("«Err»(inner)", "type"),
            ("«Ok»(Shape", "type"),
            ("Shape::«WIDE»(twice", "constant"),
            ("«wrap»(first)", "function"),
            ("{ «inner», ..", "property"),
        ],
    );
}

#[test]
fn rust_constants_may_be_one_letter_or_start_with_an_underscore() {
    let text = "fn fill<const N: usize>() -> [u8; N] {\n    [_FILL; N]\n}\nfn id<T>(value: T) -> T {\n    value\n}\n";
    check_captures(
        "rust",
        text,
        &[
            ("const «N»", "constant"),
            ("[u8; «N»]", "constant"),
            ("«_FILL»", "constant"),
            // A type parameter is still a type, whatever its length.
            ("id<«T»>", "type"),
            ("value: «T»", "type"),
        ],
    );
}

#[test]
fn python() {
    check(
        "python",
        &[
            ("#!/usr/bin/env python3", "comment"),
            ("# trailing comment", "comment"),
            ("\"\"\"Module docstring.\"\"\"", "string"),
            ("\"square\"", "string"),
            ("f\"area ", "string"),
            ("height}«\\n»\"", "string.escape"),
            ("«import» os", "keyword"),
            ("from", "keyword"),
            ("class", "keyword"),
            ("def", "keyword"),
            ("«return» len", "keyword"),
            ("«for» index", "keyword"),
            ("if width «else»", "keyword"),
            ("«LIMIT» = 10", "constant"),
            ("LIMIT = «10»", "number"),
            ("2.5", "number"),
            ("True", "boolean"),
            ("-> «None»", "constant.builtin"),
            ("class «Shape»", "type.class"),
            ("def «__init__»", "function"),
            ("def «area»", "function"),
            ("def «main»", "function"),
            ("@staticmethod", "attribute.builtin"),
            // Calling a class is the colour of the class.
            ("«Shape»(\"square\")", "type.class"),
            ("«print»(label)", "function.builtin"),
            ("«len»(label)", "function.builtin"),
            ("«range»(LIMIT)", "type.builtin"),
            ("«str»(index)", "type.builtin"),
            ("os.path.«join»(", "function.method"),
            ("os.«path».join", "property"),
            ("self.«name» =", "property"),
            ("name: «str»", "type.builtin"),
            ("-> «bool»", "type.builtin"),
            ("argv: «List»[str]", "type"),
            ("(self, «name»: str)", "variable.parameter"),
            ("«height»=2.5", "variable.parameter"),
            ("{«width» * height}", "variable"),
            ("«{»width", "punctuation.special"),
            ("width« »* height}", "embedded"),
            ("width «*» height}", "operator"),
            ("LIMIT «=» 10", "operator"),
            ("label) «if» width", "keyword"),
        ],
    );
}

#[test]
fn javascript() {
    check(
        "javascript",
        &[
            ("// A comment.", "comment"),
            ("\"node:fs\"", "string"),
            ("\"square\"", "string"),
            ("`area ", "string"),
            ("«import» {", "keyword"),
            ("«const» LIMIT", "keyword"),
            ("class", "keyword"),
            ("extends", "keyword"),
            ("export", "keyword"),
            ("«function» main", "keyword"),
            ("«return» width", "keyword"),
            ("new", "keyword"),
            ("const «LIMIT»", "constant"),
            ("LIMIT = «10»", "number"),
            ("2.5", "number"),
            ("true", "boolean"),
            ("null", "constant.builtin"),
            ("undefined", "constant.builtin"),
            ("class «Shape»", "type.class"),
            ("extends «Base»", "type.class"),
            ("new «Shape»", "type.class"),
            ("constructor", "constructor"),
            ("«area»(width, height = 2.5)", "function.method"),
            ("shape.«area»(", "function.method"),
            ("console.«log»", "function.method"),
            ("function «main»", "function"),
            ("const «double»", "function"),
            ("«double»(LIMIT)", "function"),
            ("{«main»()}", "function"),
            ("super", "variable.builtin"),
            ("«this».name", "variable.builtin"),
            ("«console».log", "variable.builtin"),
            ("this.«name»", "property"),
            ("label.«length»", "property"),
            ("const «label»", "variable"),
            ("«${»width", "punctuation.special"),
            ("${«width» * height}", "variable.parameter"),
            ("LIMIT «=» 10", "operator"),
            ("«!==» undefined", "operator"),
            ("<«div» className", "tag.jsx"),
            ("</«div»>", "tag.jsx"),
            ("className", "attribute.jsx"),
            ("\"app\"", "string"),
        ],
    );
}

#[test]
fn javascript_regex_literals_are_highlighted_as_regex() {
    check(
        "javascript",
        &[
            ("/«ab»+c/", "string"),
            ("/ab«+»c/", "operator"),
            ("/ab+c/«gi»", "keyword.operator.regex"),
        ],
    );
}

#[test]
fn typescript() {
    check(
        "typescript",
        &[
            ("// A comment.", "comment"),
            ("\"./reader\"", "string"),
            ("\"two\"", "string"),
            ("`area ", "string"),
            ("«import» type", "keyword"),
            ("import «type»", "keyword"),
            ("«export» interface", "keyword"),
            ("interface", "keyword"),
            ("readonly", "keyword"),
            ("enum", "keyword"),
            ("abstract", "keyword"),
            ("«class» Base", "keyword"),
            ("implements", "keyword"),
            ("private", "keyword"),
            ("«function» main", "keyword"),
            ("«return» width", "keyword"),
            ("interface «Shape»", "type"),
            ("type «Id»", "type"),
            ("enum «Color»", "variable"),
            ("class «Base»", "type"),
            ("{ «Reader» }", "type"),
            ("ids: «Id»[]", "type"),
            ("name: «string»;", "type.builtin"),
            ("width: «number»)", "type.builtin"),
            ("): «boolean»", "type.builtin"),
            ("readonly «name»: string;", "property"),
            ("ids.«length»", "property"),
            ("Red = «1»", "number"),
            ("2.5", "number"),
            ("true", "boolean"),
            ("constructor", "constructor"),
            ("«area»(width: number, scale", "function.method"),
            ("console.«log»", "function.method"),
            ("function «main»", "function"),
            ("(«width»: number, scale", "variable.parameter"),
            ("«scale»?: number", "variable.parameter"),
            ("(«argv»: string[]", "variable.parameter"),
            ("const «label»", "variable"),
        ],
    );
}

#[test]
fn typescript_references_to_a_parameter_keep_its_capture() {
    // The locals queries make a reference take the highlight of its definition.
    check(
        "typescript",
        &[
            ("area ${«width»}", "variable.parameter"),
            ("label, «scale»)", "variable.parameter"),
            ("return «width» *", "variable.parameter"),
            ("console.log(«label»", "variable"),
        ],
    );
}

#[test]
fn tsx() {
    check(
        "tsx",
        &[
            ("// A comment.", "comment"),
            ("\"react\"", "string"),
            ("\"card\"", "string"),
            ("«import» React", "keyword"),
            ("interface", "keyword"),
            ("export", "keyword"),
            ("«function» Card", "keyword"),
            ("«return» (", "keyword"),
            ("const", "keyword"),
            ("interface «Props»", "type"),
            ("}: «Props»)", "type"),
            ("title: «string»", "type.builtin"),
            ("count?: «number»", "type.builtin"),
            ("«title»: string", "property"),
            ("count = «0»", "number"),
            // A component is a function, whatever its capital letter says.
            ("function «Card»", "function"),
            ("console.«log»", "function.method"),
            ("<«section» className", "tag.jsx"),
            ("</«section»>", "tag.jsx"),
            ("<«h1»>", "tag.jsx"),
            ("<«Badge» value", "tag.component.jsx"),
            ("className", "attribute.jsx"),
            ("onClick", "attribute.jsx"),
            ("«value»={count}", "attribute.jsx"),
            ("<h1>{«label»}", "variable"),
        ],
    );
}

#[test]
fn json() {
    check(
        "json",
        &[
            ("// A comment, as in a settings file.", "comment"),
            ("\"name\"", "property.json_key"),
            ("\"version\"", "property.json_key"),
            ("\"depth\"", "property.json_key"),
            ("\"bakedit\"", "string"),
            ("\"editor\"", "string"),
            ("\"terminal", "string"),
            ("terminal«\\n»\"", "string.escape"),
            ("1.5", "number"),
            ("\"depth\": «2»", "number"),
            ("true", "boolean"),
            ("null", "constant.builtin"),
        ],
    );
}

#[test]
fn toml() {
    check(
        "toml",
        &[
            ("# A comment.", "comment"),
            ("«name» =", "property"),
            ("«edition» =", "property"),
            ("«target».dir", "property"),
            ("target.«dir»", "property"),
            ("{ «version» =", "property"),
            ("[«package»]", "type"),
            ("[[«bin»]]", "type"),
            ("[«dependencies»]", "type"),
            ("\"bakedit\"", "string"),
            ("'b'", "string"),
            ("\"src/main.rs\"", "string"),
            ("version = «1»\n", "number"),
            ("1.5", "number"),
            ("true", "boolean"),
            ("2026-11-22", "string.special"),
            ("name «=» \"bakedit\"", "operator"),
            ("«[»package", "punctuation.bracket"),
        ],
    );
}

#[test]
fn yaml() {
    check(
        "yaml",
        &[
            ("# A comment.", "comment"),
            ("apiVersion", "property"),
            ("«name»: \"bakedit\"", "property"),
            ("«replicas»:", "property"),
            ("v1", "string"),
            ("ConfigMap", "string"),
            ("\"bakedit\"", "string"),
            ("'second'", "string"),
            ("- «first»", "string"),
            ("echo hi", "string"),
            ("replicas: «3»", "number"),
            ("1.5", "number"),
            ("true", "boolean"),
            ("null", "constant.builtin"),
            ("&«labels»", "label"),
            ("*«labels»", "label"),
            ("«&»labels", "punctuation.special"),
            ("apiVersion«:»", "punctuation.delimiter"),
        ],
    );
}

#[test]
fn markdown() {
    check(
        "markdown",
        &[
            ("«#» Bakedit", "title.markup"),
            ("Bakedit ", "title"),
            ("Setext", "title"),
            ("«======»", "title.markup"),
            ("*«notes»*", "emphasis"),
            ("*«italic»*", "emphasis"),
            ("**«bold»**", "emphasis.strong"),
            ("«**»bold", "emphasis.strong"),
            ("inline code", "string"),
            ("«`»inline code", "string"),
            ("[«link»]", "link_text"),
            ("https://example.com", "link_uri"),
            ("\"tip\"", "string"),
            ("<https://auto.link>", "link_uri"),
            ("«- »item one", "punctuation.special"),
            ("\\*", "none"),
            ("«> »A quote.", "punctuation.special"),
            ("«```»rust", "punctuation.embedded.markup"),
            ("    indented code", "none"),
            ("Some ", ""),
            ("item one", ""),
        ],
    );
}

#[test]
fn markdown_inline_markup_inside_a_heading_wins_over_the_heading() {
    check(
        "markdown",
        &[
            ("# Bakedit *«notes»*", "emphasis"),
            ("## `«Code»` first", "string"),
            ("## «`»Code", "string"),
            ("## `Code`« first»", "title"),
        ],
    );
}

#[test]
fn shell() {
    check(
        "shell",
        &[
            ("#!/usr/bin/env bash", "keyword.directive"),
            ("# A comment.", "comment"),
            ("\"world\"", "string"),
            ("'literal'", "string"),
            ("\"hello ", "string"),
            ("heredoc ", "string"),
            ("«for» i", "keyword"),
            ("i «in» $(", "keyword"),
            ("; «do»\n", "keyword"),
            ("«if» [[", "keyword"),
            ("then", "keyword"),
            ("«fi»\n", "keyword"),
            ("done", "keyword"),
            ("«case» \"", "keyword"),
            ("esac", "keyword"),
            ("«set» -euo", "function"),
            ("«greet»() {", "function"),
            ("«greet» \"$NAME\"", "function"),
            ("«echo» \"hello", "function"),
            ("$(«seq» 1", "function"),
            ("«tr» a-z", "function"),
            // As in Zed: a variable's name is plain, a bare word is a string.
            ("«NAME»=\"world\"", "variable"),
            ("-n \"$«NAME»\"", "variable"),
            ("${«who»}", "variable"),
            ("\"$«1»\"", "variable"),
            ("-euo", "constant"),
            ("pipefail", "string"),
            ("tr «a-z»", "string"),
            ("echo «ok»", "string"),
            ("«local» who", "keyword"),
            ("«-n» \"$NAME\"", "keyword.operator"),
            ("seq «1»", "number"),
            (">&«2»", "number"),
            ("«world»)", "string.regex"),
            ("NAME«=»\"world\"", "operator"),
            ("\"«$»1\"", "punctuation.special"),
            ("\"$NAME\" «|» tr", "operator"),
            ("«${»who", "punctuation.special"),
            ("«$(»seq", "punctuation.special"),
            ("«[[» -n", "punctuation.bracket"),
            ("ok «;;»", ""),
            ("world«)»", "punctuation.delimiter"),
        ],
    );
}

#[test]
fn go() {
    check(
        "go",
        &[
            ("// Package main is a sample.", "comment"),
            ("/* block */", "comment"),
            ("\"fmt\"", "string"),
            ("\"p", "string"),
            ("\"p«\\n»\"", "string.escape"),
            ("package", "keyword"),
            ("import", "keyword"),
            ("const", "keyword"),
            ("«type» Point", "keyword"),
            ("struct", "keyword"),
            ("«func» (p", "keyword"),
            ("«return» float64", "keyword"),
            ("«if» len", "keyword"),
            ("type «Point»", "type"),
            ("X, Y «int»", "type.builtin"),
            ("Area() «float64»", "type.builtin"),
            ("«Name» string", "property"),
            ("p.«X»*", "property"),
            ("point.«Name»)", "property"),
            ("Limit = «10»", "number"),
            ("1.5", "number"),
            ("true", "boolean"),
            ("nil", "constant.builtin"),
            ("func «main»", "function"),
            ("«make»(", "function"),
            ("«len»(items)", "function"),
            ("«float64»(p.X", "function"),
            (") «Area»() float64", "function.method"),
            ("fmt.«Println»", "function.method"),
            ("strings.«ToUpper»", "function.method"),
            ("point.«Area»()", "function.method"),
            ("«point» :=", "variable"),
            ("point «:=»", "operator"),
            ("== 0 «&&» true", "operator"),
        ],
    );
}

#[test]
fn c() {
    check(
        "c",
        &[
            ("// A comment.", "comment"),
            ("/* block */", "comment"),
            ("<stdio.h>", "string"),
            ("\"local.h\"", "string"),
            ("\"«%d %s»", "string"),
            ("%s«\\n»\"", "string.escape"),
            ("«#include» <stdio.h>", "keyword"),
            ("«#define» LIMIT", "keyword"),
            ("typedef", "keyword"),
            ("«struct» point", "keyword"),
            ("static", "keyword"),
            ("«return» a + b", "keyword"),
            ("«if» (argc", "keyword"),
            ("const", "keyword"),
            ("#define «LIMIT»", "constant"),
            ("argc > «LIMIT»", "constant"),
            ("#define «SQUARE»", "function.special"),
            ("struct «point»", "type"),
            ("«int» x;", "type"),
            ("} «point_t»;", "type"),
            ("int «x»;", "property"),
            ("p.«name»)", "property"),
            ("static int «add»", "function"),
            ("int «main»", "function"),
            ("«printf»(", "function"),
            ("«add»(p.x", "function"),
            (".x = «1»", "number"),
            ("0x1F", "number"),
            // A character is a string, as in Zed, not a number.
            ("'('", "string"),
            ("LIMIT «&&» true", "operator"),
            ("argc «>» LIMIT", "operator"),
            ("add(p.x«,» 2)", "punctuation.delimiter"),
            ("true", "boolean"),
            ("int «a», int b", "variable"),
            ("a «+» b", "operator"),
        ],
    );
}

#[test]
fn cpp() {
    check(
        "cpp",
        &[
            ("// A comment.", "comment"),
            ("// namespace geo", "comment"),
            ("<string>", "string"),
            ("R\"(«raw text»)\"", "string"),
            ("«#include» <string>", "keyword"),
            ("«namespace» geo {", "keyword"),
            ("template", "keyword"),
            ("typename", "keyword"),
            ("«class» Shape", "keyword"),
            ("public", "keyword"),
            ("explicit", "keyword"),
            ("«virtual» ~", "keyword"),
            ("private", "keyword"),
            ("constexpr", "keyword"),
            ("«return» value", "keyword"),
            ("class «Shape»", "type"),
            ("typename «T»", "type"),
            ("virtual «double»", "type"),
            ("«auto» twice", "type"),
            ("std::«string» name)", "type"),
            ("explicit «Shape»(", "function"),
            ("double «area»()", "function"),
            ("auto «twice»", "function"),
            ("int «main»", "function"),
            ("items.«size»()", "function"),
            ("geo::«twice»(", "function.call"),
            ("std::«move»(", "function.call"),
            ("std::string «name_»;", "property"),
            ("kLimit = «10»", "number"),
            ("true", "boolean"),
            ("«nullptr» ==", "constant.builtin"),
            ("int «value») ->", "variable"),
        ],
    );
}

#[test]
fn html() {
    check(
        "html",
        &[
            ("<!-- A comment. -->", "comment"),
            ("<!DOCTYPE html", "constant"),
            ("<«html» lang", "tag"),
            ("<«h1» id", "tag"),
            ("</«h1»>", "tag"),
            ("<«script»>", "tag"),
            ("lang", "attribute"),
            ("charset", "attribute"),
            ("«class»=\"page\"", "attribute"),
            ("\"«en»\"", "string"),
            ("\"«page»\"", "string"),
            ("«<»html", "punctuation.bracket"),
            ("Hello", ""),
        ],
    );
}

#[test]
fn css() {
    check(
        "css",
        &[
            ("/* A comment. */", "comment"),
            ("\"base.css\"", "string"),
            ("\"Fira Code\"", "string"),
            ("\"http\"", "string"),
            ("@import", "keyword"),
            ("@media", "keyword"),
            ("!important", "keyword"),
            ("«body»,", "tag"),
            ("> «h1»", "tag"),
            // Selectors are selectors, which the built-in theme leaves plain.
            (".«card»", "selector.class"),
            ("#«main»", "selector.id"),
            ("«color»:", "property"),
            ("«margin»:", "property"),
            ("min-width", "property"),
            ("«var»(", "function"),
            ("«calc»(", "function"),
            ("var(«--accent»)", "variable"),
            (":«hover»", "selector.pseudo"),
            ("::«before»", "selector.pseudo"),
            ("[«href»", "attribute"),
            ("margin: «0»", "number"),
            ("«100»%", "number"),
            ("0.5", "number"),
            ("100«%»", "type"),
            ("2«em»", "type"),
            ("#«82aaff»", "string.special"),
            (".card «>» h1", "operator"),
        ],
    );
}

#[test]
fn swift() {
    check(
        "swift",
        &[
            ("// A comment.", "comment"),
            ("/// Doc comment.", "comment.documentation"),
            ("\"p", "string"),
            ("\"p«\\n»\"", "string.escape"),
            ("\"square\"", "string"),
            ("\"area ", "string"),
            ("import", "keyword.import"),
            ("protocol", "keyword"),
            ("«var» sides", "keyword"),
            ("«let» sides", "keyword"),
            ("«func» area", "keyword.function"),
            ("struct", "keyword.type"),
            ("enum", "keyword.type"),
            ("«return» Double", "keyword.return"),
            ("guard", "keyword.conditional"),
            ("«for» index", "keyword.repeat"),
            ("«case» red", "keyword"),
            ("protocol «Shape»", "type"),
            ("struct «Point»", "type"),
            ("sides: «Int»", "type"),
            ("-> «Double»\n}", "type"),
            ("func «area»() -> Double\n}", "function.method"),
            ("func «main»", "function.method"),
            ("«print»(", "function.call"),
            ("point.«area»()", "function.call"),
            ("«Point»(name: \"square\")", "function.call"),
            ("init", "constructor"),
            ("«self».name", "variable.builtin"),
            ("self.«name»", "property"),
            ("«name»: String) {", "variable.parameter"),
            ("sides = «0»", "number"),
            ("2.5", "number.float"),
            ("false", "boolean"),
            ("true", "boolean"),
            ("«\\(»scale", "punctuation.special"),
            ("\\(«scale»)", "variable"),
            ("0«..<»count", "operator"),
            ("«->» Bool", "operator"),
        ],
    );
}

#[test]
fn java() {
    check(
        "java",
        &[
            ("// A comment.", "comment"),
            ("/** Doc comment. */", "comment"),
            ("\"square", "string"),
            ("\"square«\\n»\"", "string.escape"),
            ("'c'", "string"),
            ("package", "keyword"),
            ("import", "keyword"),
            ("«public» class", "keyword"),
            ("«class» Shape", "keyword"),
            ("implements", "keyword"),
            ("«private» static", "keyword"),
            ("final", "keyword"),
            ("«return» name", "keyword"),
            ("«new» Shape", "keyword"),
            ("«if» (shapes", "keyword"),
            ("class «Shape»", "type"),
            ("«Comparable»<", "type"),
            ("final «String» name", "type"),
            ("«System».out", "type"),
            ("final «int»", "type.builtin"),
            ("«void» main", "type.builtin"),
            ("int «LIMIT»", "constant"),
            ("LIMIT = «10»", "number"),
            ("1.5", "number"),
            ("true", "boolean"),
            ("null", "constant.builtin"),
            ("«compareTo»(Shape other)", "function.method"),
            ("void «main»", "function.method"),
            ("name.«compareTo»(", "function.method"),
            ("out.«println»", "function.method"),
            ("«this».name", "variable.builtin"),
            ("@«Override»", "attribute"),
            ("Shape «other»)", "variable"),
        ],
    );
}

#[test]
fn lua() {
    check(
        "lua",
        &[
            ("-- A comment.", "comment"),
            ("--[[ block\ncomment ]]", "comment"),
            ("\"area", "string"),
            ("\"area«\\n»\"", "string.escape"),
            ("«local» M", "keyword"),
            ("«function» M.area", "keyword.function"),
            ("«return» #label", "keyword.return"),
            ("«for» i", "keyword.repeat"),
            ("«if» i %", "keyword.conditional"),
            ("«then»\n", "keyword.conditional"),
            ("0 «and» true", "keyword.operator"),
            ("local «LIMIT»", "constant"),
            ("LIMIT = «10»", "number"),
            ("2.5", "number"),
            ("true", "boolean"),
            ("nil", "constant.builtin"),
            ("function M.«area»", "function"),
            ("local function «helper»", "function"),
            ("function M:«method»", "function.method"),
            ("M.«area»(i", "function.call"),
            ("«helper»(self, LIMIT)", "function.call"),
            ("«tostring»(", "function.builtin"),
            ("«print»(", "function.builtin"),
            ("(«width», height)", "variable.parameter"),
            ("local «label»", "variable"),
            ("label «=» \"area", "operator"),
        ],
    );
}

#[test]
fn lua_references_to_a_parameter_keep_its_capture() {
    check(
        "lua",
        &[
            ("tostring(«width» * height)", "variable.parameter"),
            ("i = 1, «n» do", "variable.parameter"),
            ("print(«label»)", "variable"),
        ],
    );
}

#[test]
fn dockerfile() {
    check(
        "dockerfile",
        &[
            ("# A comment.", "comment"),
            ("\"-C target-cpu=native\"", "string"),
            ("\"/app/bakedit\"", "string"),
            ("\"--serve\"", "string"),
            ("ARG", "keyword"),
            ("FROM", "keyword"),
            ("«AS» build", "keyword"),
            ("WORKDIR", "keyword"),
            ("COPY", "keyword"),
            ("ENV", "keyword"),
            ("RUN", "keyword"),
            ("EXPOSE", "keyword"),
            ("ENTRYPOINT", "keyword"),
            ("${«VERSION»}", "constant"),
            ("«${»VERSION", "punctuation.special"),
        ],
    );
}

#[test]
fn sql() {
    check(
        "sql",
        &[
            ("-- A comment.", "comment"),
            ("/* block */", "comment"),
            ("'a%'", "string"),
            ("CREATE", "keyword"),
            ("TABLE", "keyword"),
            ("SELECT", "keyword"),
            ("«FROM» users", "keyword"),
            ("WHERE", "keyword"),
            ("«AND» o.amount", "keyword.operator"),
            ("«NOT» NULL", "keyword.operator"),
            ("TABLE «users»", "type"),
            ("INTEGER", "type.builtin"),
            ("VARCHAR", "type.builtin"),
            ("u.«name»,", "property"),
            ("o.«user_id»", "property"),
            ("COUNT", "function.call"),
            ("VARCHAR(«40»)", "number"),
            ("10.5", "number.float"),
            ("LIMIT «5»", "number"),
            ("DEFAULT «TRUE»", "boolean"),
            ("AS «total»", "variable"),
            ("o.amount «>» 10.5", "operator"),
        ],
    );
}

#[test]
fn ruby() {
    check(
        "ruby",
        &[
            ("# A comment.", "comment"),
            ("\"json\"", "string"),
            ("\"area ", "string"),
            ("height}«\\n»\"", "string.escape"),
            (":name", "string.special.symbol"),
            ("module", "keyword"),
            ("«class» Shape", "keyword"),
            ("«def» initialize", "keyword"),
            ("«end»\n\n    def area", "keyword"),
            ("n) «if» n", "keyword"),
            ("module «Geometry»", "constructor"),
            ("class «Shape»", "constructor"),
            ("«LIMIT» = 10", "constant"),
            ("LIMIT = «10»", "number"),
            ("2.5", "number"),
            ("true", "boolean"),
            ("nil", "constant.builtin"),
            ("def «initialize»", "function.method"),
            ("def «area»", "function.method"),
            ("self.«build»", "function.method"),
            ("require", "function.method.builtin"),
            ("attr_reader", "function.method"),
            ("«puts» label", "function.method"),
            ("«new»(\"square\")", "function.method"),
            ("width.«zero?»", "function.method"),
            ("shape.«area»(n)", "function.method"),
            ("@name", "property"),
            ("def «self».build", "variable.builtin"),
            ("initialize(«name»)", "variable.parameter"),
            ("«#{»width", "punctuation.special"),
        ],
    );
}

#[test]
fn ruby_tells_a_local_variable_from_a_method_call() {
    // A bare identifier is a method call unless the locals query saw it defined.
    check(
        "ruby",
        &[
            ("«label» = \"area", "variable"),
            ("puts «label»", "variable"),
            ("«puts» label", "function.method"),
            ("@name = «name»", "variable.parameter"),
            ("#{«width» * height}", "variable.parameter"),
            ("«shape».area(n)", "variable"),
        ],
    );
}

#[test]
fn kotlin() {
    check(
        "kotlin",
        &[
            ("// A comment.", "comment"),
            ("/* block */", "comment"),
            ("\"p\\n\"", "string"),
            ("\"area ", "string"),
            ("import", "keyword.import"),
            ("const", "keyword"),
            ("«val» LIMIT", "keyword"),
            ("data", "keyword"),
            ("«class» Point", "keyword"),
            ("interface", "keyword"),
            ("override", "keyword"),
            ("«fun» area(): Double\n}", "keyword.function"),
            ("«if» (args", "keyword.conditional"),
            ("return", "keyword.return"),
            ("class «Point»", "type"),
            ("interface «Shape»", "type"),
            ("x: «Int»", "type.builtin"),
            ("LIMIT = «10»", "number"),
            ("2.5", "number.float"),
            ("true", "boolean"),
            ("fun «area»(): Double\n}", "function"),
            // The bundled query misses a function with a modifier in front of it.
            ("override fun «area»", "function"),
            ("fun «main»", "function"),
            ("«max»(radius", "function"),
            ("«Circle»(1.0)", "function"),
            ("shape.«area»()", "function"),
            ("println", "function.builtin"),
            ("val «x»: Int", "property"),
            ("args.«size»", "property"),
            ("(«args»: Array", "variable.parameter"),
            ("«${»radius", "punctuation.special"),
            ("${«radius» * scale}", "variable"),
        ],
    );
}

#[test]
fn diff() {
    check(
        "diff",
        &[
            ("«diff» --git", "function"),
            ("diff «--git»", "variable.parameter"),
            ("«index» 83db48f", "keyword"),
            ("100644", "number"),
            ("83db48f", "constant"),
            ("bf2a1c3", "constant"),
            // The markers are punctuation; what follows them carries the colour.
            ("«---» a/src/main.rs", "punctuation.special"),
            ("--- «a/src/main.rs»", "keyword"),
            ("+++ «b/src/main.rs»", "string"),
            ("@@ -1,4 +1,4 @@", "attribute"),
            ("«-»    println!(\"old\");", "punctuation.special"),
            ("-«    println!(\"old\");»", "keyword"),
            ("+«    println!(\"new\");»", "string"),
            (" fn main() {", ""),
        ],
    );
}

#[test]
fn make() {
    check(
        "make",
        &[
            ("# A comment.", "comment"),
            ("clang", "string"),
            ("-O2 -Wall", "string"),
            ("«CC» :=", "constant.builtin"),
            ("«CFLAGS» +=", "constant.builtin"),
            ("«OBJECTS» =", "constant"),
            ("$(«DEBUG»)", "constant"),
            ("CC «:=»", "operator"),
            ("patsubst", "keyword.function"),
            ("wildcard", "keyword.function"),
            ("ifeq", "keyword.conditional"),
            ("endif", "keyword.conditional"),
            ("include", "keyword.import"),
            (".PHONY", "constant.macro"),
            ("«all»: bakedit", "constant.macro"),
            ("«clean»:\n", "constant.macro"),
            ("«%.o»: %.c", "string.regex"),
            ("«$»(OBJECTS)\n\t", "punctuation.special"),
        ],
    );
}

#[test]
fn nix() {
    check(
        "nix",
        &[
            ("# A comment.", "comment"),
            ("/* block */", "comment"),
            ("\"1.0\"", "string"),
            ("\"bakedit\"", "string"),
            ("\"An editor", "string"),
            ("An editor«\\n»\"", "string.escape"),
            ("<nixpkgs>", "string.special.path"),
            ("./.", "string.special.path"),
            ("«let»\n", "keyword"),
            ("«in»\npkgs", "keyword"),
            ("rec", "keyword"),
            ("inherit", "keyword"),
            ("«with» pkgs", "keyword"),
            ("«version» = \"1.0\"", "property"),
            ("pname", "property"),
            ("pkgs.«stdenv»", "property"),
            ("limit = «10»", "number"),
            ("1.5", "number"),
            ("{ «pkgs» ?", "variable.parameter"),
            ("«x»: x * 2", "variable.parameter"),
            ("x: «x» * 2", "variable"),
            ("«import» <nixpkgs>", "function.builtin"),
            ("mkDerivation", "function"),
            ("x «*» 2", "operator"),
        ],
    );
}

#[test]
fn nix_build_phases_are_highlighted_as_shell() {
    check(
        "nix",
        &[
            ("«cargo» build", "function"),
            ("--release", "constant"),
            ("«echo» ${", "function"),
            ("«${»toString", "punctuation.special"),
            ("toString", "function.builtin"),
            ("(«double» limit)", "function"),
        ],
    );
}

#[test]
fn zig() {
    check(
        "zig",
        &[
            ("//! Module docs.", "comment.documentation"),
            ("// A comment.", "comment"),
            ("\"std\"", "string"),
            ("\"p", "string"),
            ("\"p«\\n»\"", "string.escape"),
            ("«const» std", "keyword"),
            ("«var» point", "keyword"),
            ("struct", "keyword.type"),
            ("pub", "keyword.modifier"),
            ("«fn» area", "keyword.function"),
            ("«return» @as", "keyword.return"),
            ("«if» (point", "keyword.conditional"),
            ("«for» (0", "keyword.repeat"),
            ("0 «and» true", "keyword.operator"),
            ("@import", "keyword.import"),
            ("const «std»", "type.namespace"),
            ("const «Point»", "type"),
            ("«usize» = 10", "type.builtin"),
            ("x: «i32»", "type.builtin"),
            // `#lua-match?` in the bundled query.
            ("const «LIMIT»", "constant"),
            ("0..«LIMIT»", "constant"),
            ("LIMIT: usize = «10»", "number"),
            ("2.5", "number.float"),
            ("true", "boolean"),
            ("fn «area»", "function"),
            ("fn «main»", "function"),
            ("debug.«print»", "function.call"),
            ("point.«area»(ratio)", "function.call"),
            ("@as", "function.builtin"),
            ("@floatFromInt", "function.builtin"),
            ("«x»: i32", "property"),
            ("self.«x»", "property"),
            ("(«self»: Point", "variable.parameter"),
            ("«scale»: f64", "variable.parameter"),
            ("var «point»", "variable"),
        ],
    );
}

#[test]
fn php() {
    check(
        "php",
        &[
            ("// A comment.", "comment"),
            ("/* block */", "comment"),
            ("'literal'", "string"),
            ("\"round\"", "string"),
            ("<?php", "tag"),
            ("?>", "tag"),
            ("«namespace» App", "keyword"),
            ("«use» App", "keyword"),
            ("«const» LIMIT", "keyword"),
            ("final", "keyword"),
            ("«class» Circle", "keyword"),
            ("extends", "keyword"),
            ("private", "keyword"),
            ("«public» function", "keyword"),
            ("«function» __construct", "keyword"),
            ("echo", "keyword"),
            ("«return» $width", "keyword"),
            ("«new» Circle", "keyword"),
            ("namespace «App»", "type.namespace"),
            ("Base\\«Shape»;", "type"),
            ("private «float»", "type.builtin"),
            ("(«string» $name)", "type.builtin"),
            ("const «LIMIT»", "constant"),
            ("LIMIT = «10»", "number"),
            ("2.5", "number"),
            ("true", "boolean"),
            ("null", "constant.builtin"),
            ("__construct", "function.method"),
            ("function «area»", "function.method"),
            ("$shape->«area»(", "function.method"),
            ("function «main»", "function"),
            ("strlen", "function"),
            ("«count»($argv)", "function"),
            ("new «Circle»", "constructor"),
            ("$«radius» = 2.5", "property"),
            ("$this->«name»", "property"),
            ("$«this»->name", "variable.builtin"),
            ("string $«name»)", "variable"),
            ("«$»radius", "operator"),
        ],
    );
}

#[test]
fn php_text_outside_the_tags_is_highlighted_as_html() {
    check(
        "php",
        &[
            ("<«h1»>Title", "tag"),
            ("</«h1»>", "tag"),
            ("<«p» class", "tag"),
            ("<p «class»", "attribute"),
            ("\"«footer»\"", "string"),
        ],
    );
}

#[test]
fn csharp() {
    check(
        "csharp",
        &[
            ("// A comment.", "comment"),
            ("/* block */", "comment"),
            ("\"c", "string"),
            ("\"c«\\n»\"", "string.escape"),
            ("\"round\"", "string"),
            ("«using» System;", "keyword"),
            ("«namespace» Sample", "keyword"),
            ("«public» interface", "keyword"),
            ("interface", "keyword"),
            ("«class» Circle", "keyword"),
            ("private", "keyword"),
            ("const", "keyword"),
            ("«return» scale", "keyword"),
            ("«new» Circle", "keyword"),
            ("«var» scale", "keyword"),
            ("static", "keyword"),
            ("namespace «Sample»", "type.namespace"),
            ("interface «IShape»", "type"),
            ("class «Circle»", "type"),
            ("new «Circle»", "type"),
            ("«double» Area();", "type.builtin"),
            ("const «int»", "type.builtin"),
            ("Limit = «10»", "number"),
            ("2.5", "number"),
            ("true", "boolean"),
            ("null", "constant.builtin"),
            ("double «Area»();", "function"),
            ("void «Main»", "function"),
            ("Console.«WriteLine»", "function"),
            ("shape.«Area»()", "function"),
            ("public «Circle»(", "constructor"),
            ("string «name»)", "variable.parameter"),
            ("string[] «args»", "variable.parameter"),
            ("var «scale»", "variable"),
        ],
    );
}

#[test]
fn dart() {
    check(
        "dart",
        &[
            ("// A comment.", "comment"),
            ("/// Doc comment.", "comment.documentation"),
            ("'dart:math'", "string"),
            ("\"c", "string"),
            ("\"c«\\n»\"", "string.escape"),
            ("'area ", "string"),
            ("import", "keyword"),
            ("«const» limit", "keyword"),
            ("abstract", "keyword"),
            ("«class» Shape", "keyword"),
            ("implements", "keyword"),
            ("«final» double", "keyword"),
            ("«return» max", "keyword"),
            ("«if» (args", "keyword"),
            ("«var» nothing", "keyword"),
            ("class «Shape»", "type"),
            ("class «Circle»", "type"),
            ("final «Shape» shape", "type"),
            ("«double» area();", "type.builtin"),
            ("«void» main", "type.builtin"),
            ("limit = «10»", "number"),
            ("2.5", "number.float"),
            ("true", "boolean"),
            ("null", "constant.builtin"),
            ("double «area»();", "function"),
            ("void «main»", "function"),
            ("double «area»() {", "function.method"),
            ("«print»(", "function.call"),
            ("«max»(radius", "function.call"),
            ("shape.«area»()", "function.method.call"),
            ("@override", "attribute"),
            ("«Circle»(this.radius)", "constructor"),
            ("args.«length»", "property"),
            ("List<String> «args»", "variable.parameter"),
            ("«${»radius", "punctuation.special"),
        ],
    );
}

#[test]
fn dart_references_take_the_capture_of_their_definition() {
    check(
        "dart",
        &[
            ("if («args».length", "variable.parameter"),
            ("${«radius» * scale}", "variable"),
            ("«shape».area()", "variable"),
            ("> «limit» &&", "variable"),
        ],
    );
}

#[test]
fn go_mod() {
    check(
        "gomod",
        &[
            ("// A comment.", "comment"),
            ("// indirect", "comment"),
            ("module", "keyword"),
            ("«go» 1.23", "keyword"),
            ("require", "keyword"),
            ("replace", "keyword"),
            ("1.23", "string"),
            ("v1.2.3", "string"),
            ("v0.20.0", "string"),
            ("=>", "operator"),
            ("example.com/bakedit", ""),
        ],
    );
}

#[test]
fn git_commit() {
    check(
        "git_commit",
        &[
            ("feat", "keyword"),
            ("(«syntax»)", "variable.parameter"),
            (" add the highlighter", "title"),
            ("BREAKING CHANGE: ", "comment.error"),
            ("spans are byte offsets", "none"),
            ("Signed-off-by: ", "label"),
            (
                "# Please enter the commit message for your changes. Lines starting",
                "comment",
            ),
            ("# On branch ", "comment"),
            ("feat/syntax", "link_text"),
            ("Changes to be committed:", "title"),
            ("modified:", "keyword"),
            ("new file:", "keyword"),
            ("crates/syntax/src/lib.rs", "string.special.url"),
            ("A longer body", ""),
        ],
    );
}

#[test]
fn every_language_with_a_grammar_has_a_sample_and_every_sample_is_checked() {
    let checked = include_str!("languages.rs");
    for language in bakedit_syntax::languages()
        .iter()
        .filter(|language| language.has_grammar())
    {
        assert!(
            SAMPLES.iter().any(|sample| sample.language == language.id),
            "{} has a grammar but no sample",
            language.id
        );
        assert!(
            checked.contains(&format!("check(\n        \"{}\",", language.id)),
            "{} has a sample but no expectations",
            language.id
        );
    }
    for sample in SAMPLES {
        assert!(
            common::language(sample.language).has_grammar(),
            "{}",
            sample.language
        );
    }
}

#[test]
fn every_sample_produces_spans_and_no_sample_is_all_one_capture() {
    for sample in SAMPLES {
        let spans = highlight(sample.language, sample.text);
        let mut names: Vec<&str> = spans.iter().map(|span| span.capture.name()).collect();
        names.sort_unstable();
        names.dedup();
        assert!(names.len() >= 4, "{}: only {names:?}", sample.language);
    }
}
