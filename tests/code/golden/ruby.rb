require "net/http"
require_relative "lib/helper"

MAX_RETRIES = 3
TIMEOUT = 30

module Utilities
  class Parser
    def parse(input)
    end
  end
end

class Animal
  def initialize(name)
  end

  def speak
  end
end

class Dog < Animal
  def self.create(name)
  end

  def fetch(item)
  end
end

def standalone(x, y)
end
