//! Frozen dataclass-like Python classes (positional/keyword constructor,
//! read-only fields, dataclass equality, hash and repr).

#[macro_export]
macro_rules! record_class {
    ($rust:ident, $pyname:literal, $module:literal, [$($f:ident),+ $(,)?]) => {
        #[cfg(feature = "python")]
        #[pyo3::pyclass(module = $module, name = $pyname, frozen, subclass)]
        pub struct $rust {
            $(#[pyo3(get)] pub $f: pyo3::Py<pyo3::PyAny>,)+
        }

        impl $rust {
            #[cfg(feature = "python")]
            fn fields<'py>(&self, py: pyo3::Python<'py>) -> pyo3::PyResult<pyo3::Bound<'py, pyo3::types::PyTuple>> {
                pyo3::types::PyTuple::new(py, [$(self.$f.bind(py).clone()),+])
            }
        }

        #[cfg(feature = "python")]
        #[pyo3::pymethods]
        impl $rust {
            #[cfg(feature = "python")]
            #[new]
            #[pyo3(signature = ($($f),+))]
            fn new($($f: pyo3::Py<pyo3::PyAny>),+) -> Self {
                $rust { $($f),+ }
            }

            #[cfg(feature = "python")]
            fn __repr__(&self, py: pyo3::Python<'_>) -> pyo3::PyResult<String> {
                let mut parts: Vec<String> = Vec::new();
                $( parts.push(format!("{}={}", stringify!($f), self.$f.bind(py).repr()?)); )+
                Ok(format!("{}({})", $pyname, parts.join(", ")))
            }

            #[cfg(feature = "python")]
            fn __eq__(slf: &pyo3::Bound<'_, Self>, other: &pyo3::Bound<'_, pyo3::PyAny>) -> pyo3::PyResult<pyo3::Py<pyo3::PyAny>> {
                #[cfg(feature = "python")]
                use pyo3::prelude::*;
                let py = slf.py();
                match other.cast::<$rust>() {
                    Ok(o) if o.get_type().is(slf.get_type()) => {
                        let eq = slf.get().fields(py)?.eq(o.get().fields(py)?)?;
                        Ok(pyo3::types::PyBool::new(py, eq).to_owned().into_any().unbind())
                    }
                    _ => Ok(py.NotImplemented()),
                }
            }

            #[cfg(feature = "python")]
            fn __hash__(&self, py: pyo3::Python<'_>) -> pyo3::PyResult<isize> {
                #[cfg(feature = "python")]
                use pyo3::prelude::*;
                self.fields(py)?.hash()
            }
        }
    };
}
