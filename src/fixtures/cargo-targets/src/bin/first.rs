fn run() -> usize { 1 }

fn main() {
    let _local = run();
    let _library = cargo_targets_fixture::run();
}
