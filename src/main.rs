fn main() {
    println!("{}", greeting());
}

fn greeting() -> &'static str {
    "hello from Smart-Claude-Router"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn greeting_is_not_empty() {
        assert!(!greeting().is_empty());
    }
}
