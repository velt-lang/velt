use super::*;
use Ty::*;

fn s(fields: &[(Ty, u32)], size: u32, align: u32) -> StructShape {
    StructShape {
        fields: fields.to_vec(),
        size,
        align,
    }
}

fn plan(conv: Conv, args: &[Arg], ret: Option<&StructShape>) -> Plan {
    classify(conv, args, ret)
}

fn one(conv: Conv, shape: StructShape) -> Pass {
    plan(conv, &[Arg::Struct(shape)], None).args.remove(0)
}

fn words(w: &[Ty]) -> Pass {
    Pass::Words(w.to_vec())
}

const LINUX_ARM: Conv = Conv::Aapcs64 { apple: false };
const APPLE_ARM: Conv = Conv::Aapcs64 { apple: true };

/// The classes clang gives these shapes on x86_64-unknown-linux-gnu.
#[test]
fn sysv_eightbytes_are_integer_or_sse() {
    let one = |shape| one(Conv::SysV, shape);
    assert_eq!(one(s(&[(F64, 0), (F64, 8)], 16, 8)), words(&[F64, F64]));
    assert_eq!(one(s(&[(I64, 0), (F64, 8)], 16, 8)), words(&[I64, F64]));
    assert_eq!(one(s(&[(F32, 0), (I32, 4)], 8, 4)), words(&[I64]));
    assert_eq!(one(s(&[(F32, 0), (F32, 4)], 8, 4)), words(&[F64]));
    assert_eq!(
        one(s(&[(F32, 0), (F32, 4), (F32, 8)], 12, 4)),
        words(&[F64, F64])
    );
    assert_eq!(one(s(&[(U8, 0)], 1, 1)), words(&[I64]));
    assert_eq!(one(s(&[(Bool, 0), (F32, 4)], 8, 4)), words(&[I64]));
    assert_eq!(one(s(&[(Ptr, 0), (F64, 8)], 16, 8)), words(&[I64, F64]));
    // Over 16 bytes: on the stack.
    assert_eq!(
        one(s(&[(I64, 0), (I64, 8), (I64, 16)], 24, 8)),
        Pass::ByVal { size: 24, align: 8 }
    );
}

/// A struct takes all its registers or none: with one integer register left, a two-INTEGER
/// struct goes on the stack, and the register stays for the scalar after it.
#[test]
fn sysv_a_struct_takes_its_registers_all_or_none() {
    let pair = Arg::Struct(s(&[(I64, 0), (I64, 8)], 16, 8));
    let mut args = vec![Arg::Scalar(I64); 5];
    args.push(pair.clone());
    args.push(Arg::Scalar(I32));
    let p = plan(Conv::SysV, &args, None);
    assert_eq!(p.args[5], Pass::ByVal { size: 16, align: 8 });
    assert_eq!(p.args[6], Pass::Value);
    // With four used, it fits.
    let mut args = vec![Arg::Scalar(I64); 4];
    args.push(pair);
    assert_eq!(plan(Conv::SysV, &args, None).args[4], words(&[I64, I64]));
    // Floats count against the vector registers only.
    let floats = Arg::Struct(s(&[(F64, 0), (F64, 8)], 16, 8));
    let mut args = vec![Arg::Scalar(F64); 7];
    args.push(floats);
    assert_eq!(
        plan(Conv::SysV, &args, None).args[7],
        Pass::ByVal { size: 16, align: 8 }
    );
}

/// A result of 16 bytes or less comes back in registers; a larger one in memory, whose pointer
/// takes the first integer register.
#[test]
fn sysv_results_in_registers_or_memory() {
    let small = s(&[(F64, 0), (I32, 8)], 16, 8);
    assert_eq!(
        plan(Conv::SysV, &[], Some(&small)).ret,
        Ret::Words(vec![F64, I64])
    );
    let big = s(&[(I64, 0), (I64, 8), (I64, 16)], 24, 8);
    let pair = Arg::Struct(s(&[(I64, 0), (I64, 8)], 16, 8));
    let mut args = vec![Arg::Scalar(I64); 4];
    args.push(pair);
    let p = plan(Conv::SysV, &args, Some(&big));
    assert_eq!(p.ret, Ret::Sret { size: 24, align: 8 });
    // The sret pointer took a register: five left, four used, one short of the pair.
    assert_eq!(p.args[4], Pass::ByVal { size: 16, align: 8 });
}

/// What clang gives these shapes on x86_64-pc-windows-msvc: 1, 2, 4 and 8 bytes in an integer
/// register (floats too), anything else by reference; results the same or through `sret`.
#[test]
fn win64_passes_integers_of_one_two_four_or_eight_bytes_and_the_rest_by_reference() {
    let one = |shape| one(Conv::Win64, shape);
    assert_eq!(one(s(&[(U8, 0)], 1, 1)), words(&[I64]));
    assert_eq!(one(s(&[(I16, 0)], 2, 2)), words(&[I64]));
    assert_eq!(one(s(&[(F32, 0)], 4, 4)), words(&[I64]));
    assert_eq!(one(s(&[(F32, 0), (F32, 4)], 8, 4)), words(&[I64]));
    assert_eq!(one(s(&[(F64, 0)], 8, 8)), words(&[I64]));
    assert_eq!(one(s(&[(U8, 0), (U8, 1), (U8, 2)], 3, 1)), Pass::Indirect);
    assert_eq!(
        one(s(&[(I16, 0), (U16, 2), (I16, 4)], 6, 2)),
        Pass::Indirect
    );
    assert_eq!(one(s(&[(F64, 0), (F64, 8)], 16, 8)), Pass::Indirect);
    let r = |shape: StructShape| plan(Conv::Win64, &[], Some(&shape)).ret;
    assert_eq!(r(s(&[(F64, 0)], 8, 8)), Ret::Words(vec![I64]));
    assert_eq!(r(s(&[(U8, 0)], 1, 1)), Ret::Words(vec![I64]));
    assert_eq!(
        r(s(&[(I32, 0), (I32, 4), (I32, 8)], 12, 4)),
        Ret::Sret { size: 12, align: 4 }
    );
    // Every argument has its position's register, whatever came before.
    let mut args = vec![Arg::Struct(s(&[(F64, 0), (F64, 8)], 16, 8)); 5];
    args.push(Arg::Struct(s(&[(I32, 0)], 4, 4)));
    let p = plan(Conv::Win64, &args, None);
    assert_eq!(p.args[4], Pass::Indirect);
    assert_eq!(p.args[5], words(&[I64]));
}

/// What clang gives these shapes on aarch64-unknown-linux-gnu: an HFA in vector registers, one
/// field each; other structs up to 16 bytes as eightbytes; larger ones by reference.
#[test]
fn aapcs64_passes_hfas_in_vector_registers_and_the_rest_as_eightbytes() {
    for conv in [LINUX_ARM, APPLE_ARM] {
        let one = |shape| one(conv, shape);
        assert_eq!(one(s(&[(F32, 0)], 4, 4)), words(&[F32]));
        assert_eq!(
            one(s(&[(F32, 0), (F32, 4), (F32, 8)], 12, 4)),
            words(&[F32, F32, F32])
        );
        let four_doubles = s(&[(F64, 0), (F64, 8), (F64, 16), (F64, 24)], 32, 8);
        assert_eq!(one(four_doubles), words(&[F64; 4]));
        // Mixed float types, five floats, or an integer among them: no HFA.
        assert_eq!(one(s(&[(F32, 0), (F64, 8)], 16, 8)), words(&[I64, I64]));
        let five = (0..5).map(|k| (F32, 4 * k)).collect::<Vec<_>>();
        assert_eq!(one(s(&five, 20, 4)), Pass::Indirect);
        assert_eq!(one(s(&[(F32, 0), (I32, 4)], 8, 4)), words(&[I64]));
        assert_eq!(one(s(&[(U8, 0)], 1, 1)), words(&[I64]));
        assert_eq!(
            one(s(&[(I64, 0), (I64, 8), (I64, 16)], 24, 8)),
            Pass::Indirect
        );
    }
}

/// A struct that doesn't fit in the registers left goes on the stack and uses up the rest of
/// its class's registers, so a scalar after it goes on the stack too; the other class's stay.
#[test]
fn aapcs64_a_struct_that_does_not_fit_uses_up_its_class() {
    let pair = Arg::Struct(s(&[(I64, 0), (I64, 8)], 16, 8));
    let mut args = vec![Arg::Scalar(I64); 7];
    args.push(pair.clone());
    args.push(Arg::Scalar(F64));
    let p = plan(LINUX_ARM, &args, None);
    assert_eq!(
        p.args[7],
        Pass::Stack {
            pad: vec![I64],
            words: vec![I64, I64]
        }
    );
    assert_eq!(p.args[8], Pass::Value);
    // Six used: it fits.
    assert_eq!(
        plan(LINUX_ARM, &args[1..], None).args[6],
        words(&[I64, I64])
    );
    // An f32 HFA on the stack: its bytes as vector eightbytes on Linux, its fields on Apple.
    let quad = Arg::Struct(s(&[(F32, 0), (F32, 4), (F32, 8), (F32, 12)], 16, 4));
    let mut args = vec![Arg::Scalar(F64); 6];
    args.push(quad);
    let stack = |conv| plan(conv, &args, None).args.remove(6);
    assert_eq!(
        stack(LINUX_ARM),
        Pass::Stack {
            pad: vec![F64, F64],
            words: vec![F64, F64]
        }
    );
    assert_eq!(
        stack(APPLE_ARM),
        Pass::Stack {
            pad: vec![F64, F64],
            words: vec![F32; 4]
        }
    );
    // A struct passed by reference takes one integer register.
    let big = Arg::Struct(s(&[(I64, 0), (I64, 8), (I64, 16)], 24, 8));
    let mut args = vec![Arg::Scalar(I64); 6];
    args.push(big);
    args.push(pair);
    let p = plan(LINUX_ARM, &args, None);
    assert_eq!(p.args[6], Pass::Indirect);
    assert_eq!(
        p.args[7],
        Pass::Stack {
            pad: vec![I64],
            words: vec![I64, I64]
        }
    );
}

/// Results come back as they would be passed; a larger one through x8, which leaves the
/// argument registers alone.
#[test]
fn aapcs64_results_in_registers_or_through_x8() {
    let r = |shape: StructShape| plan(LINUX_ARM, &[], Some(&shape)).ret;
    assert_eq!(
        r(s(&[(F32, 0), (F32, 4), (F32, 8)], 12, 4)),
        Ret::Words(vec![F32; 3])
    );
    assert_eq!(r(s(&[(I32, 0), (F32, 4)], 8, 4)), Ret::Words(vec![I64]));
    let big = s(&[(I64, 0), (I64, 8), (I64, 16)], 24, 8);
    let mut args = vec![Arg::Scalar(I64); 6];
    args.push(Arg::Struct(s(&[(I64, 0), (I64, 8)], 16, 8)));
    let p = plan(LINUX_ARM, &args, Some(&big));
    assert_eq!(p.ret, Ret::Sret { size: 24, align: 8 });
    assert_eq!(p.args[6], words(&[I64, I64]));
}

#[test]
fn conventions_by_target() {
    assert_eq!(conv("x86_64-unknown-linux-gnu"), Some(Conv::SysV));
    assert_eq!(conv("x86_64-apple-darwin"), Some(Conv::SysV));
    assert_eq!(conv("x86_64-pc-windows-msvc"), Some(Conv::Win64));
    assert_eq!(conv("x86_64-pc-windows-gnu"), Some(Conv::Win64));
    assert_eq!(conv("aarch64-unknown-linux-gnu"), Some(LINUX_ARM));
    assert_eq!(conv("aarch64-unknown-linux-musl"), Some(LINUX_ARM));
    assert_eq!(conv("aarch64-apple-darwin"), Some(APPLE_ARM));
    assert_eq!(conv("aarch64-pc-windows-msvc"), None);
    assert_eq!(conv("wasm32-wasip1"), None);
}
