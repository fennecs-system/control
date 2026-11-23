fn main() {
    control_discord::bridge::command(&std::env::args().collect::<Vec<_>>());
}
