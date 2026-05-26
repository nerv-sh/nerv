fn main() {
    use nerv_engine::{complete, SpecRegistry};
    let r = SpecRegistry::at_dir(std::path::Path::new("/Users/tak/Library/Caches/nerv/specs"));
    let res = complete("git ", 4, &r);
    println!("count: {}", res.items.len());
    for s in res.items.iter().take(45) {
        println!("- {}", s.display);
    }
}
