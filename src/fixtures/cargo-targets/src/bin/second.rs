fn run() -> usize { 2 }

fn main() {
    let _local = run();
    let _library = cargo_targets_fixture::run();
}
