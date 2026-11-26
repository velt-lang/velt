// The Node twin of tests/golden/std/data_view_subclass_setters.velt, without the types.
class Loud extends DataView {
  setUint16(byteOffset, value, littleEndian = false) {
    console.log("setUint16");
    super.setUint16(byteOffset, value, littleEndian);
  }

  setUint32(byteOffset, value, littleEndian = false) {
    console.log("setUint32");
    super.setUint32(byteOffset, value, littleEndian);
  }
}

const d = new Loud(new ArrayBuffer(8));
d.setInt16(0, -2);
d.setInt32(4, -3, true);
console.log(d.getInt16(0), d.getUint16(0), d.getInt32(4, true));
d.setUint16(0, 7);
d.setUint32(4, 9, true);
console.log(d.getUint16(0), d.getUint32(4, true));
