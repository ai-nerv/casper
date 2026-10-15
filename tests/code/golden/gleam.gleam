import gleam/io
import gleam/option.{Some, None} as opt

pub const max_size = 1024

pub type Shape {
  Circle(Float)
  Rectangle(Float, Float)
}

type Color =
  String

pub fn greet(name: String) -> String {
  "Hello, " <> name
}

fn helper(x: Int) -> Int {
  x + 1
}
