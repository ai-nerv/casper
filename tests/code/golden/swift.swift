import Foundation
import UIKit

public class Vehicle {
    public var name: String
    public init(name: String) {}
    public func start() {}
}

public struct Point {
    var x: Double
    var y: Double
}

public enum Direction {
    case north
    case south
}

public protocol Drawable {
    func draw()
}

extension Vehicle: Drawable {
    func draw() {}
}

public func process(input: String) -> Bool { return true }

let MAX_COUNT = 100
