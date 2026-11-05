import five, { v } from "./a";

const yes: boolean = false;

export const early: number = yes ? five + 1 : 0;

export const earlyNamed: number = yes ? v + 1 : 0;

export const earlyLogical: boolean = yes && five > 1;
