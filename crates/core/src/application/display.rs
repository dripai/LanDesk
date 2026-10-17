use crate::domain::*;
use crate::ports::*;
use anyhow::Result;
/// Preparing is transactional: retain the old display until capture of the new
/// one has succeeded. Committing keeps it alive across browser disconnects.
pub struct VirtualDisplays {
    provider: Box<dyn VirtualDisplayProvider>,
    current: Option<(VirtualDisplaySpec, std::rc::Rc<dyn VirtualDisplay>)>,
}
impl VirtualDisplays {
    pub fn new(provider: impl VirtualDisplayProvider + 'static) -> Self {
        Self {
            provider: Box::new(provider),
            current: None,
        }
    }
    pub fn prepare(&self, spec: VirtualDisplaySpec) -> Result<std::rc::Rc<dyn VirtualDisplay>> {
        let spec = spec.validate()?;
        if let Some((current, display)) = &self.current
            && *current == spec
        {
            return Ok(display.clone());
        }
        self.provider.create(spec)
    }
    pub fn commit(&mut self, spec: VirtualDisplaySpec, display: std::rc::Rc<dyn VirtualDisplay>) {
        self.current = Some((spec, display));
    }
    pub fn find(&self, id: u32) -> Option<std::rc::Rc<dyn VirtualDisplay>> {
        self.current
            .as_ref()
            .filter(|(_, d)| d.id() == id)
            .map(|(_, d)| d.clone())
    }
    pub fn current_id(&self) -> Option<u32> {
        self.current.as_ref().map(|(_, display)| display.id())
    }
}
