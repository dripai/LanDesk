use anyhow::Result;
type Undo = (&'static str, Box<dyn FnOnce() -> Result<()>>);
#[derive(Default)]
pub struct Transaction {
    undo: Vec<Undo>,
}
impl Transaction {
    pub fn new() -> Self {
        Self { undo: Vec::new() }
    }
    pub fn record(&mut self, label: &'static str, action: impl FnOnce() -> Result<()> + 'static) {
        self.undo.push((label, Box::new(action)));
    }
    /// Continue every independent restoration, reporting all failures instead of masking them.
    pub fn rollback(mut self, failure: anyhow::Error) -> anyhow::Error {
        let mut errors = vec![format!("{failure:#}")];
        for (label, action) in self.undo.drain(..).rev() {
            if let Err(error) = action() {
                errors.push(format!("回滚 {label} 失败: {error:#}"));
            }
        }
        anyhow::anyhow!(errors.join("；"))
    }
}
