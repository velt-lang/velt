"use strict";
class D {
    t;
    constructor(t) {
        this.t = t;
    }
    total() {
        return this.t;
    }
}
class S {
    total() {
        return 7;
    }
    tag() {
        return "s";
    }
}
class Sq {
    name() {
        return "sq";
    }
}
class Big extends Sq {
    name() {
        return "big";
    }
}
let calls = 0;
class Impl {
    n() {
        return new D(1);
    }
    u() {
        return new D(2);
    }
    b() {
        return new D(3);
    }
    bn(k) {
        return k == 0 ? null : new D(4);
    }
    bu(k) {
        return k == 0 ? undefined : new D(5);
    }
    nul() {
        calls++;
        return null;
    }
    nulb() {
        calls++;
        return null;
    }
    undb() {
        calls++;
        return undefined;
    }
    und() {
        calls++;
        return undefined;
    }
    same() {
        return new D(6);
    }
    sameb(k) {
        return k == 0 ? null : new D(8);
    }
    sq() {
        return new Big();
    }
    sqb(k) {
        return k == 0 ? undefined : new Big();
    }
    sub() {
        return new S();
    }
    subb(k) {
        return k == 0 ? null : new S();
    }
}
function show(f) {
    return f === null ? "null" : f === undefined ? "undefined" : "" + f.total();
}
function showSq(f) {
    return f === null ? "null" : f === undefined ? "undefined" : f.name();
}
class GF {
    next(k) {
        return k == 0 ? null : new S();
    }
    one() {
        return new S();
    }
}
class GenG {
    v;
    constructor(v) {
        this.v = v;
    }
    next(k) {
        return k == 0 ? null : this.v;
    }
    one() {
        return this.v;
    }
}
function main() {
    const g = new Impl();
    console.log(show(g.n()), show(g.u()), show(g.b()));
    console.log(show(g.bn(0)), show(g.bn(1)), show(g.bu(0)), show(g.bu(1)));
    console.log(show(g.nul()), show(g.nulb()), show(g.undb()), show(g.und()), calls);
    console.log(show(g.same()), show(g.sameb(0)), show(g.sameb(1)));
    console.log(showSq(g.sq()), showSq(g.sqb(0)), showSq(g.sqb(1)));
    console.log(show(g.sub()), show(g.subb(0)), show(g.subb(1)));
    const q = new GF();
    console.log(q.next(0), q.next(1)?.total(), q.one()?.total());
    const h = new GenG("x");
    console.log(h.next(0), h.next(1), h.one());
}
main();
