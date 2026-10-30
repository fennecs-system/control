fn main() {
    control::discord::bridge::command(&std::env::args().collect::<Vec<_>>());
}
