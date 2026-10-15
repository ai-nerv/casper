import 'package:flutter/material.dart';

class Person {
  String name;
  int age;

  Person(this.name, this.age);

  String greet() {
    return "Hello, $name";
  }

  int get yearsUntil100 => 100 - age;
}

enum Mood { happy, sad }

String topLevelGreet(String name) {
  return "Hello, $name";
}
