//! What Lo's own queries add to each grammar's.

use symbol_index::FileSymbols;

use crate::support::{GO, JAVASCRIPT, PYTHON, RUST, TSX, TYPESCRIPT, definitions, references};

#[test]
fn rust() {
    let source = r#"
pub trait Shape {
    type Unit;
    const SIDES: u32;
    fn area(&self) -> f64;
}

pub enum Kind {
    Round,
    Square,
}

pub struct Circle {
    radius: f64,
}

static COUNT: AtomicUsize = AtomicUsize::new(0);

impl Circle {
    pub fn new() -> Circle {
        Circle { radius: 1.0 }
    }

    fn kind(&self) -> Kind {
        let radius = self.radius;
        let _ = std::mem::size_of::<Circle>();
        tracing::info!("{radius}");
        Kind::Round
    }
}
"#;
    let symbols = FileSymbols::read(&RUST, source.as_bytes());

    assert_eq!(
        definitions(&symbols),
        [
            "interface Shape",
            "type Unit < Shape",
            "constant SIDES < Shape",
            "method area < Shape",
            "class Kind",
            "variant Round < Kind",
            "variant Square < Kind",
            "class Circle",
            "field radius < Circle",
            "constant COUNT",
            "method new",
            "method kind",
        ]
    );
    assert_eq!(
        references(&symbols),
        [
            "type AtomicUsize in COUNT",
            "path AtomicUsize in COUNT",
            "call new in COUNT",
            "implementation Circle",
            "type Circle in new",
            "type Circle in new",
            "field radius in new",
            "type Kind in kind",
            "field radius in kind",
            "path std in kind",
            "path mem in kind",
            "call size_of in kind",
            "type Circle in kind",
            "path tracing in kind",
            "call info in kind",
            "path Kind in kind",
            "path Round in kind",
        ]
    );
}

#[test]
fn typescript() {
    let source = r#"
export type Id = string | number;

export enum Color {
  Red,
}

namespace Geometry {
  export function unit() {}
}

const LIMIT = 10;
export const shapes: Shape[] = [];

interface Shape {
  id: Id;
}

class Circle extends Base implements Shape {
  id: Id = LIMIT;
  color = Color.Red;
}
"#;
    let symbols = FileSymbols::read(&TYPESCRIPT, source.as_bytes());

    assert_eq!(
        definitions(&symbols),
        [
            "type Id",
            "enum Color",
            "module Geometry",
            "function unit < Geometry",
            "constant LIMIT",
            "constant shapes",
            "interface Shape",
            "field id < Shape",
            "class Circle",
            "field id < Circle",
            "field color < Circle",
        ]
    );
    assert_eq!(
        references(&symbols),
        [
            "type Shape in shapes",
            "type Id in id",
            "class Base in Circle",
            "type Shape in Circle",
            "type Id in id",
        ]
    );
}

#[test]
fn tsx_and_javascript_components() {
    let source = r#"
export function App() {
  return (
    <Layout>
      <div />
      <Button label="ok" />
    </Layout>
  );
}
"#;
    for grammar in [&TSX, &JAVASCRIPT] {
        let symbols = FileSymbols::read(grammar, source.as_bytes());

        // Lowercase tags are HTML elements, not components.
        assert_eq!(
            references(&symbols),
            ["call Layout in App", "call Button in App"]
        );
    }
}

#[test]
fn python() {
    let source = r#"
@dataclass
class Circle(shapes.Shape, Base):
    radius: float

    @functools.cache
    def area(self) -> Area:
        return Area(self.radius)

def unit(size: Optional[Circle]) -> list[Circle]:
    pass
"#;
    let symbols = FileSymbols::read(&PYTHON, source.as_bytes());

    assert_eq!(
        definitions(&symbols),
        ["class Circle", "function area < Circle", "function unit"]
    );
    assert_eq!(
        references(&symbols),
        [
            "call dataclass",
            "class Shape in Circle",
            "class Base in Circle",
            "type float in Circle",
            "call cache in Circle",
            "type Area in area",
            "call Area in area",
            "type Circle in unit",
            "type Circle in unit",
        ]
    );
}

#[test]
fn go() {
    let source = r#"
package shapes

const (
	Pi  = 3.14
	Tau = Pi * 2
)

var Unit = Circle{Radius: 1}

var (
	zero Circle
)

type Shape interface {
	Area() float64
}

type Circle struct {
	Radius float64
}

func (c Circle) Area() float64 {
	const local = 2
	return Pi * c.Radius * c.Radius
}
"#;
    let symbols = FileSymbols::read(&GO, source.as_bytes());

    assert_eq!(
        definitions(&symbols),
        [
            "constant Pi",
            "constant Tau",
            "variable Unit",
            "variable zero",
            "type Shape",
            "method Area < Shape",
            "type Circle",
            "field Radius < Circle",
            "method Area",
        ]
    );
    assert_eq!(
        references(&symbols),
        [
            "type Circle in Unit",
            "type Circle in zero",
            "type float64 in Area",
            "type float64 in Radius",
            "type Circle in Area",
            "type float64 in Area",
            "field Radius in Area",
            "field Radius in Area",
        ]
    );
}
