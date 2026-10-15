<?php
namespace App\Services;

use App\Models\User;

const VERSION = "1.0";

class UserService extends BaseService implements Serializable
{
    private string $name;
    public function __construct(string $name) {}
    public function find(int $id): ?User {}
    public static function create(array $data): self {}
}

interface Repository
{
    public function getById(int $id): mixed;
    public function save(object $entity): void;
}

trait Loggable
{
}

function helper(string $input): string {}

enum Status
{
    case Active;
    case Inactive;
}
