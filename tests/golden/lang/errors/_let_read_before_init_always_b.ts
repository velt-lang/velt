import { v } from "./_let_read_before_init_always_a";

export const early: number = v + 1;

export const also: number = (v > 1 ? 1 : 2) + 1;
