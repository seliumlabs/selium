pub trait Operations {
    fn map(self, module_path: &str) -> Self;
    fn filter(self, module_path: &str) -> Self;
}
