module sample

import os
import encoding.json as json

pub const max_size = 1024

pub struct Point {
  x f64
  y f64
}

pub interface Drawable {
  draw() string
}

pub enum Color {
  red
  green
  blue
}

pub type Number = int | f64

pub fn add(a int, b int) int {
  return a + b
}

pub fn (point Point) distance() f64 {
  return point.x + point.y
}

fn main() {
  println(os.args)
}
