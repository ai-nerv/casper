"""Rules for the build."""

load("@bazel_skylib//lib:paths.bzl", "paths")

VISIBILITY = ["//visibility:public"]

def _impl(ctx):
    out = ctx.actions.declare_file(ctx.label.name)
    return [DefaultInfo(files = depset([out]))]

stamp = rule(
    implementation = _impl,
    attrs = {"src": attr.label(allow_single_file = True)},
)

def stamped(name, src, **kwargs):
    stamp(name = name, src = src, **kwargs)
