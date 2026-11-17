const fs=['exp','expm1','log','log1p','log2','log10','sin','cos','tan','asin','acos','atan','sinh','cosh','tanh','asinh','acosh','atanh','cbrt','fround'];
let s=987654321>>>0; const r=()=>{s^=s<<13;s>>>=0;s^=s>>>17;s^=s<<5;s>>>=0;return s;};
const N=3000; const xs=[];
for(let i=0;i<N;i++){const m=i%4; let x;
 if(m==0){const b=new BigUint64Array([ (BigInt(r())<<32n)|BigInt(r()) ]); x=new Float64Array(b.buffer)[0];}
 else if(m==1) x=(r()/4294967296*2-1)*800; else if(m==2) x=(r()/4294967296*2-1)*1.5; else x=(r()/4294967296)*1e6*(r()&1?1:-1);
 xs.push(x);}
const bits=x=>{const b=new BigUint64Array(new Float64Array([x]).buffer)[0]; return x!==x?'nan':b.toString(16)};
const mode=process.argv[2];
if(mode==='gen'){ const inb=xs.map(x=>'0x'+new BigUint64Array(new Float64Array([x]).buffer)[0].toString(16));
 let out=`const XS: u64[] = [${inb.join(',')}];\nfunction bits(x: f64): string { return x != x ? "nan" : x.toBits().toString(16); }\nfunction main() {\n  for (const b of XS) {\n    const x = f64.fromBits(b);\n`;
 for(const f of fs) out+=`    console.log(\`${f} \${bits(Math.${f}(x))}\`);\n`;
 out+=`    console.log(\`atan2 \${bits(Math.atan2(x, f64.fromBits(XS[(b % 2999) as usize])))}\`);\n  }\n}\n`; console.log(out);}
else { for(const x of xs){ for(const f of fs) console.log(`${f} ${bits(Math[f](x))}`);
  const b=new BigUint64Array(new Float64Array([x]).buffer)[0]; console.log(`atan2 ${bits(Math.atan2(x, xs[Number(b%2999n)]))}`);} }
