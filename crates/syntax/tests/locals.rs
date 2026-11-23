//! Local variables: a reference is highlighted as its definition was, and patterns that say
//! "not a local" leave locals alone.

mod common;

use common::check_captures as check;

#[test]
fn a_javascript_local_named_like_a_builtin_is_not_a_builtin() {
    let text = "\
console.log(module);

function shadow(console) {
  const module = console;
  return module;
}
";
    check(
        "javascript",
        text,
        &[
            // At the top level these are the globals.
            ("«console».log(module)", "variable.builtin"),
            ("console.log(«module»)", "variable.builtin"),
            // Inside, they are a parameter and a constant.
            ("shadow(«console»)", "variable.parameter"),
            ("const «module»", "variable"),
            ("= «console»;", "variable.parameter"),
            ("return «module»", "variable"),
        ],
    );
}

#[test]
fn a_local_that_is_called_is_a_call() {
    // A reference takes its definition's colour only where the syntax has nothing more
    // specific to say than "a variable".
    let text = "function apply(callback, value) {\n  return callback(value);\n}\n";
    check(
        "javascript",
        text,
        &[
            ("apply(«callback»,", "variable.parameter"),
            ("return «callback»(", "function"),
            ("callback(«value»)", "variable.parameter"),
        ],
    );
}

#[test]
fn a_javascript_function_keeps_its_colour_where_it_is_used() {
    let text = "\
const double = (x) => x * 2;
const plain = 3;
export default [double, plain];
";
    check(
        "javascript",
        text,
        &[
            ("const «double»", "function"),
            // Not called here, but still the function defined above.
            ("[«double»,", "function"),
            ("const «plain»", "variable"),
            (", «plain»]", "variable"),
        ],
    );
}

#[test]
fn a_ruby_method_does_not_see_the_locals_around_it() {
    let text = "\
total = 1

def report
  total
end

puts total
[1].each { |n| total + n }
";
    check(
        "ruby",
        text,
        &[
            ("«total» = 1", "variable"),
            // A method body is a fresh scope: here `total` can only be a method call.
            ("def report\n  «total»", "function.method"),
            // Back outside, and inside a block, it is the variable again.
            ("puts «total»", "variable"),
            ("«total» + n", "variable"),
            ("|«n»|", "variable.parameter"),
            ("total + «n»", "variable.parameter"),
        ],
    );
}

#[test]
fn a_ruby_block_parameter_ends_with_its_block() {
    let text = "[1].each { |item| item }\nitem\n";
    check(
        "ruby",
        text,
        &[
            ("|«item»|", "variable.parameter"),
            ("| «item» }", "variable.parameter"),
            // After the block there is no such local: a bare name is a method call.
            ("}\n«item»", "function.method"),
        ],
    );
}

#[test]
fn a_lua_function_name_is_known_inside_and_a_parameter_ends_with_its_function() {
    let text = "\
local function fact(n)
  if n < 2 then return 1 end
  return n * fact(n - 1)
end
print(n)
";
    check(
        "lua",
        text,
        &[
            ("function «fact»", "function"),
            ("(«n»)", "variable.parameter"),
            ("if «n» <", "variable.parameter"),
            // A reference to the function defined above, and written as a call: the syntax
            // is the more specific of the two.
            ("«fact»(n - 1)", "function.call"),
            // The parameter is out of scope here.
            ("print(«n»)", "variable"),
        ],
    );
}

#[test]
fn typescript_parameters_shadow_and_unshadow() {
    let text = "\
function outer(value: number) {
  const inner = (value: string) => value.length;
  return inner(String(value)) + value;
}
";
    check(
        "typescript",
        text,
        &[
            ("outer(«value»: number)", "variable.parameter"),
            ("(«value»: string)", "variable.parameter"),
            ("=> «value».length", "variable.parameter"),
            ("String(«value»)", "variable.parameter"),
            ("+ «value»;", "variable.parameter"),
            ("const «inner»", "function"),
            ("return «inner»(", "function"),
        ],
    );
}
