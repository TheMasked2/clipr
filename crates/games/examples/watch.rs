//! Prints the detected game every 2 s for 10 s. The game list cache goes to the temp directory.

use std::time::Duration;

use clipr_games::GameWatcher;

fn main() {
    let cache = std::env::temp_dir()
        .join("clipr-games-example")
        .join("games.json");
    println!("cache: {}", cache.display());
    let watcher = GameWatcher::new(cache);
    for _ in 0..5 {
        println!("known exes: {}, game: {:?}", watcher.list_len(), watcher.poll());
        std::thread::sleep(Duration::from_secs(2));
    }
    println!("known exes: {}", watcher.list_len());
}
