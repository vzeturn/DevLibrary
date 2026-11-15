// Prints the expected float formatting vectors used in src/ser/tests.rs.
// Run with: node crates/dl-json/testdata/float_vectors.js
// (on Windows PowerShell: Get-Content crates/dl-json/testdata/float_vectors.js -Raw | node -)
// Each line is `<f64 bits as hex> <JSON.stringify output>`, so the inputs are exact.
const inputs = [
  0.1, 0.5, 1.5, -1.5, 100, 123.456, -123.456, 0.1 + 0.2, 1 / 3, 2 / 3, 4.35,
  1e20, 1e21, 1.5e21, 123456789012345680000, 9007199254740993, 2 ** 53, 2 ** 64,
  1e-6, 1e-7, 1.5e-7, 0.000001234, 1.5e-8, 1e-5, 0.00001, 5e-324,
  2.2250738585072014e-308, 1.7976931348623157e308, 1e100, -1e-100, 1e308,
  123e-20, 0.000123, 1234567.125, 299792458, 6.02214076e23, 1.602176634e-19,
  3.141592653589793, 2.718281828459045, 1e16, 12345678901234567890,
];
const buf = new DataView(new ArrayBuffer(8));
for (const x of inputs) {
  buf.setFloat64(0, x);
  const hex = buf.getBigUint64(0).toString(16).padStart(16, "0");
  console.log(hex + " " + JSON.stringify(x));
}
