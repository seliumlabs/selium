pub const fn kilobyte(units: u64) -> u64 {
    units * 1024
}

pub const fn megabyte(units: u64) -> u64 {
    kilobyte(units) * 1024
}

pub const fn gigabyte(units: u64) -> u64 {
    megabyte(units) * 1024
}

