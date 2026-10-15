import { useState } from 'react';

export interface Props {
    title: string;
}

export type ID = string | number;

export class Widget {
    render(): Element { return <div />; }
}

export function App({ title }: Props) {
    const [n] = useState(0);
    return <main>{title}{n}</main>;
}
