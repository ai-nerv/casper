"""Module docstring."""

import os
from typing import Optional

MAX_RETRIES = 3
MY_VAR: int = 10


@dataclass
class MyClass:
    x: int = 0


class AuthService:
    def __init__(self, secret: str):
        self.secret = secret

    @staticmethod
    def validate(token: str) -> bool:
        return True


def process(data: list) -> dict:
    return {}


def test_process():
    assert process([]) == {}
