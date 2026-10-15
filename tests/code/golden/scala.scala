package com.example

import scala.collection.mutable.Map
import java.io.File

val MaxRetries = 3

class Service(name: String) extends Base with Logging {
  def process(input: String): Boolean = true
  def shutdown(): Unit = {}
}

object Config {
  def load(path: String): Config = ???
}

trait Handler {
  def handle(req: Request): Response
}

def helper(x: Int): String = x.toString

type Callback = String => Unit
