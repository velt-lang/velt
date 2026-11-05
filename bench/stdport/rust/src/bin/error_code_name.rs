use std::hint::black_box;

/// The runtime's `code_name`, which `velt_rt_err_code_name` wrote out before the Velt port.
#[inline(never)]
fn code_name(c: i32) -> &'static str {
    match c {
        0 => "",
        1 => "ENOENT",
        2 => "EACCES",
        3 => "EEXIST",
        4 => "EINVAL",
        5 => "EILSEQ",
        6 => "ETIMEDOUT",
        7 => "ECONNREFUSED",
        8 => "ECONNRESET",
        9 => "EADDRINUSE",
        10 => "EPIPE",
        11 => "EOF",
        12 => "ENOTSUP",
        13 => "ENOTDIR",
        14 => "ENOTEMPTY",
        15 => "EISDIR",
        16 => "EBADF",
        _ => "UNKNOWN",
    }
}

fn main() {
    let mut total: i64 = 0;
    let mut code: i32 = 0;
    for _ in 0..20_000_000 {
        total += code_name(black_box(code)).len() as i64;
        code = (code * 5 + 7) % 32;
    }
    println!("{total}");
}
