import fs from 'fs';
const path = require('path');

export const LIMIT = 10;

export class Reader {
    constructor(root) { this.root = root; }
    read(name) { return fs.readFileSync(path.join(this.root, name)); }
}

export function render(props) {
    return <section>{props.title}</section>;
}

function helper(x) {
    return x + 1;
}
