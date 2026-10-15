package com.example

import kotlin.collections.List
import java.io.File

const val MAX_SIZE = 100
val SOME_CONST = "value"

typealias StringList = List<String>

data class User(val name: String, val age: Int) : Comparable<User> {
    fun greet(): String = "Hello $name"
}

object Singleton {
    fun instance(): Singleton = this
}

fun topLevel(x: Int): Int = x

enum class Color {
    RED, GREEN, BLUE
}
