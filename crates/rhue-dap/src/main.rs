//! `rhue-dap`, the Rhue debug adapter on stdio. Everything's in the library,
//! see [`rhue_dap::serve`].

fn main() {
    rhue_dap::serve(std::io::BufReader::new(std::io::stdin()), std::io::stdout());
}
