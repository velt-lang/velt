// Array and typed array loops (#1125): `loops.ts <kernel> <reps>`. Valid TypeScript; the
// kernels print a checksum so Node and both builds can be compared.
function u8Rmw(reps: number): number {
  const u = new Uint8Array(1000000);
  let s = 0;
  for (let r = 0; r < reps; r++) {
    for (let i = 0; i < u.length; i++) {
      u[i] = (u[i] + i) & 255;
      s += u[i];
    }
  }
  return s;
}

function f64Dot(reps: number): number {
  const a = new Float64Array(2000000);
  const b = new Float64Array(2000000);
  for (let i = 0; i < a.length; i++) {
    a[i] = i * 0.5;
    b[i] = 1 / (i + 1);
  }
  let s = 0;
  for (let r = 0; r < reps; r++) {
    let d = 0;
    for (let i = 0; i < a.length; i++) {
      d += a[i] * b[i];
    }
    s += d;
  }
  return s;
}

function f64Axpy(reps: number): number {
  const a = new Float64Array(1000000);
  for (let r = 0; r < reps; r++) {
    for (let i = 0; i < a.length; i++) {
      a[i] = a[i] * 0.5 + 1;
    }
  }
  return a[0] + a[a.length - 1];
}

function i32Sum(reps: number): number {
  const a = new Int32Array(1000000);
  for (let i = 0; i < a.length; i++) {
    a[i] = i - 500000;
  }
  let s = 0;
  for (let r = 0; r < reps; r++) {
    for (let i = 0; i < a.length; i++) {
      s += a[i];
    }
  }
  return s;
}

function bytesXor(reps: number): number {
  const xs: number[] = [];
  for (let i = 0; i < 65536; i++) {
    xs.push(i & 255);
  }
  const b = new Uint8Array(xs);
  let s = 0;
  for (let r = 0; r < reps; r++) {
    for (let i = 0; i < b.length; i++) {
      b[i] = b[i] ^ ((r + i) & 255);
    }
    s += b[r & 65535];
  }
  return s;
}

function nested(reps: number): number {
  const n = 300;
  const m: number[][] = [];
  for (let i = 0; i < n; i++) {
    const row: number[] = [];
    for (let j = 0; j < n; j++) {
      row.push(i + j);
    }
    m.push(row);
  }
  let s = 0;
  for (let r = 0; r < reps; r++) {
    for (let i = 0; i < n; i++) {
      const row = m[i];
      for (let j = 0; j < n; j++) {
        row[j] = row[j] * 0.5 + r;
        s += row[j];
      }
    }
  }
  return s;
}

function main(): void {
  const kernel = process.argv.length > 2 ? process.argv[2] : "u8";
  const reps = process.argv.length > 3 ? Number(process.argv[3]) : 1;
  let s = 0;
  if (kernel === "u8") {
    s = u8Rmw(reps);
  } else if (kernel === "dot") {
    s = f64Dot(reps);
  } else if (kernel === "axpy") {
    s = f64Axpy(reps);
  } else if (kernel === "i32") {
    s = i32Sum(reps);
  } else if (kernel === "xor") {
    s = bytesXor(reps);
  } else if (kernel === "nested") {
    s = nested(reps);
  }
  console.log(kernel, s);
}
