(ns my.app
  (:require [clojure.string :as str]
            [clojure.set :refer [union]])
  (:import (java.util Date Calendar)))

(def ^:private answer 42)

(defonce cache (atom {}))

(defn greet
  "doc"
  [name]
  (str "hi " name))

(defn- helper [x] (inc x))

(defmacro with-thing [& body] `(do ~@body))

(defprotocol P
  (size [this]))

(defrecord Point [x y]
  P
  (size [this] 2))
