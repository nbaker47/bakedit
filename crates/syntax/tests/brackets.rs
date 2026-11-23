//! Bracket nesting on real highlights: which characters count as brackets in each language.

mod common;

use bakedit_syntax::bracket_marks;
use common::highlight;

/// Every mark as `(the character, depth)`, after checking each one has its partner.
fn nesting(language: &str, text: &str) -> Vec<(char, u16)> {
    let spans = highlight(language, text);
    bracket_marks(text, &spans)
        .into_iter()
        .map(|mark| {
            assert!(
                mark.matched,
                "unmatched bracket at {} in {text:?}",
                mark.byte
            );
            (text.as_bytes()[mark.byte] as char, mark.depth)
        })
        .collect()
}

fn pairs(marks: &[(char, u16)]) -> String {
    marks
        .iter()
        .map(|(character, depth)| format!("{character}{depth}"))
        .collect()
}

#[test]
fn rust_generics_and_closure_bars_are_brackets_but_comparisons_are_not() {
    let text = "fn f<T: Ord>(a: Vec<(T, u8)>) -> bool { a.iter().any(|x| x.1 < 3 || x.1 > 9) }\n";
    assert_eq!(
        pairs(&nesting("rust", text)),
        // f<T>   (a: Vec<(T, u8)>)        { iter() any( |x|  x.1 < 3 ... ) }
        "<0>0(0<1(2)2>1)0{0(1)1(1|2|2)1}0"
    );
    // Nested generics close with what the eye reads as a shift.
    assert_eq!(
        pairs(&nesting(
            "rust",
            "type M = HashMap<String, Vec<Option<u8>>>;\n"
        )),
        "<0<1<2>2>1>0"
    );
    // A shift and a bitwise or are operators.
    assert_eq!(
        pairs(&nesting("rust", "const N: u8 = (1 << 3) | (8 >> 1);\n")),
        "(0)0(0)0"
    );
}

#[test]
fn typescript_generics_are_brackets_and_tsx_leaves_angle_brackets_alone() {
    let text = "function first<T>(items: Array<[T, number]>): T { return items[0][0]; }\n";
    assert_eq!(
        pairs(&nesting("typescript", text)),
        "<0>0(0<1[2]2>1)0{0[1]1[1]1}0"
    );
    assert_eq!(
        pairs(&nesting("typescript", "const small = a < b && c > (d);\n")),
        "(0)0"
    );
    // As in Zed: where there are tags, no angle bracket is coloured, generics included.
    let tsx = "const view = (items: Array<string>) => <ul className=\"x\">{items.map((i) => <li>{i}</li>)}</ul>;\n";
    assert_eq!(pairs(&nesting("tsx", tsx)), "(0)0{0(1(2)2{2}2)1}0");
    let jsx = "const view = <div onClick={() => go([1])}>text</div>;\n";
    assert_eq!(pairs(&nesting("javascript", jsx)), "{0(1)1(1[2]2)1}0");
}

#[test]
fn cpp_templates_are_brackets_and_html_tags_are_not() {
    let text = "template <typename T> std::vector<std::pair<int, T>> make(T v) { return {{1, v}}; }\nbool lt = 1 < 2;\n";
    assert_eq!(pairs(&nesting("cpp", text)), "<0>0<0<1>1>0(0)0{0{1{2}2}1}0");
    assert_eq!(
        pairs(&nesting("c", "int lt(int a) { return a < (2 >> 1); }\n")),
        "(0)0{0(1)1}0"
    );
    let html = "<p class=\"a\">call(x)[0]</p>\n";
    assert_eq!(pairs(&nesting("html", html)), "(0)0[0]0");
}
