package com.example;

import java.util.List;
import java.io.IOException;

public class Service extends BaseService implements Runnable, Serializable {
    private String name;
    public Service(String name) { this.name = name; }
    @Override
    public String toString() { return name; }
    public void process(List<String> items) throws IOException {}
}

/** Handler docs */
public interface Handler extends Comparable<Handler> {
    void handle(String request);
}

public enum Direction implements Displayable {
    UP, DOWN, LEFT, RIGHT
}
