#include <iostream>
#include "mylib.h"

using std::string;

#define MAX_BUF 1024

namespace utils {
    void helper(int x);
}

class Shape {
public:
    virtual double area() const = 0;
    void describe();
private:
    int id;
};

struct Point {
    double x;
    double y;
};

enum Color { Red, Green, Blue };

template<typename T>
T identity(T val) { return val; }

void process(const string& input) {}

typedef unsigned long ulong;
