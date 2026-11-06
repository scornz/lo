//! A file's definitions and references, with each language's own query.

use symbol_index::{FileSymbols, Position};

use crate::support::{GO, JAVASCRIPT, PYTHON, RUST, TYPESCRIPT, definitions, references};

#[test]
fn rust() {
    let source = r#"
mod shapes {
    pub struct Circle {
        pub radius: f64,
    }
}

pub trait Area {
    fn area(&self) -> f64;
}

impl Area for shapes::Circle {
    fn area(&self) -> f64 {
        square(self.radius) * 3.14
    }
}

fn square(x: f64) -> f64 {
    x * x
}

macro_rules! log {
    ($x:expr) => {};
}

fn main() {
    let circle = shapes::Circle { radius: 1.0 };
    let area = circle.area();
    log!(area);
    println!("{area}");
}
"#;
    let symbols = FileSymbols::read(&RUST, source.as_bytes());

    assert_eq!(
        definitions(&symbols),
        [
            "module shapes",
            "class Circle < shapes",
            "interface Area",
            // An impl block isn't a definition, so its methods have no parent.
            "method area",
            "function square",
            "macro log",
            "function main",
        ]
    );
    assert_eq!(
        references(&symbols),
        [
            "implementation Area",
            "call square in area",
            "call area in main",
            "call log in main",
            "call println in main",
        ]
    );
}

#[test]
fn typescript() {
    let source = r#"
interface Shape {
  area(): number;
}

abstract class Base {
  abstract describe(): string;
}

class Circle extends Base implements Shape {
  constructor(private radius: number) {
    super();
  }
  area(): number {
    return square(this.radius) * Math.PI;
  }
  describe(): string {
    return `circle ${this.area()}`;
  }
}

function square(x: number): number {
  return x * x;
}

export const makeCircle = (r: number): Shape => new Circle(r);
"#;
    let symbols = FileSymbols::read(&TYPESCRIPT, source.as_bytes());

    assert_eq!(
        definitions(&symbols),
        [
            "interface Shape",
            "method area < Shape",
            "class Base",
            "method describe < Base",
            // JavaScript's query leaves out constructors.
            "class Circle",
            "method area < Circle",
            "method describe < Circle",
            "function square",
            "function makeCircle",
        ]
    );
    assert_eq!(
        references(&symbols),
        [
            "call square in area",
            "call area in describe",
            "type Shape in makeCircle",
            "class Circle in makeCircle",
        ]
    );
}

#[test]
fn javascript() {
    let source = r#"
const { helper } = require("./helper");

class Widget {
  constructor(name) {
    this.name = name;
  }
  render() {
    return helper(this.name);
  }
}

function* ids() {
  yield 1;
}

const make = function (name) {
  return new Widget(name);
};

module.exports.create = (name) => make(name);

export default function App() {
  return <Widget name="app" />;
}
"#;
    let symbols = FileSymbols::read(&JAVASCRIPT, source.as_bytes());

    assert_eq!(
        definitions(&symbols),
        [
            "class Widget",
            "method render < Widget",
            "function ids",
            "function make",
            "function create",
            "function App",
        ]
    );
    // JavaScript's query leaves out `require`.
    assert_eq!(
        references(&symbols),
        [
            "call helper in render",
            "class Widget in make",
            "call make in create",
        ]
    );
}

#[test]
fn python() {
    let source = r#"
import math

LIMIT = 10

class Shape:
    def area(self):
        return 0

class Circle(Shape):
    def __init__(self, radius):
        self.radius = radius

    def area(self):
        return square(self.radius) * math.pi

def square(x):
    return x * x

circle = Circle(LIMIT)
print(circle.area())
"#;
    let symbols = FileSymbols::read(&PYTHON, source.as_bytes());

    assert_eq!(
        definitions(&symbols),
        [
            "constant LIMIT",
            "class Shape",
            "function area < Shape",
            "class Circle",
            "function __init__ < Circle",
            "function area < Circle",
            "function square",
            "constant circle",
        ]
    );
    assert_eq!(
        references(&symbols),
        [
            "call square in area",
            "call Circle in circle",
            "call print",
            "call area",
        ]
    );
}

#[test]
fn go() {
    let source = r#"
package shapes

import "math"

// Shape has an area.
type Shape interface {
	Area() float64
}

type Circle struct {
	Radius float64
}

// Area is the circle's area.
func (c Circle) Area() float64 {
	return square(c.Radius) * math.Pi
}

func square(x float64) float64 {
	return x * x
}

func Unit() Shape {
	var s Shape = Circle{Radius: 1}
	fmt.Println(s.Area())
	return s
}
"#;
    let symbols = FileSymbols::read(&GO, source.as_bytes());

    assert_eq!(
        definitions(&symbols),
        [
            "type Shape",
            "type Circle",
            "method Area",
            "function square",
            "function Unit",
        ]
    );
    assert_eq!(
        references(&symbols),
        [
            "type float64 in Shape",
            "type float64 in Circle",
            "type Circle in Area",
            "type float64 in Area",
            "call square in Area",
            "type float64 in square",
            "type float64 in square",
            "type Shape in Unit",
            "type Shape in Unit",
            "type Circle in Unit",
            "call Println in Unit",
            "call Area in Unit",
        ]
    );
}

#[test]
fn where_a_definition_and_a_reference_are() {
    let source = "fn main() {\n    let s = \"é\"; greet(s);\n}\n";
    let symbols = FileSymbols::read(&RUST, source.as_bytes());

    let [main] = symbols.definitions() else {
        panic!("{:?}", symbols.definitions());
    };
    assert_eq!(
        &source[main.range.clone()],
        "fn main() {\n    let s = \"é\"; greet(s);\n}"
    );
    assert_eq!(&source[main.name_range.clone()], "main");
    assert_eq!(
        main.position,
        Position {
            row: 0,
            column: 3,
            utf16_column: 3
        }
    );

    let [greet] = symbols.references() else {
        panic!("{:?}", symbols.references());
    };
    assert_eq!(&source[greet.range.clone()], "greet");
    // "é" is two bytes and one UTF-16 code unit.
    assert_eq!(
        greet.position,
        Position {
            row: 1,
            column: 18,
            utf16_column: 17
        }
    );
}

#[test]
fn a_file_that_fails_to_parse_keeps_what_it_can() {
    let source = "fn first() {}\nfn broken( {\nfn last() {}\n";
    let symbols = FileSymbols::read(&RUST, source.as_bytes());

    assert!(definitions(&symbols).contains(&"function first".to_string()));
}

#[test]
fn an_empty_file_has_no_symbols() {
    let symbols = FileSymbols::read(&RUST, b"");

    assert!(symbols.definitions().is_empty());
    assert!(symbols.references().is_empty());
}
