Name:           scylla-cpp-rust-driver
Version:        %{driver_version}
Release:        %{driver_release}%{?dist}
Summary:        ScyllaDB Cpp-Rust Driver
Group:          Development/Tools

License:        GPL 2.1
URL:            https://github.com/scylladb/cpp-rust-driver
Source0:        %{name}-%{driver_version}-%{driver_release}.tar
BuildRequires:  rust
BuildRequires:  cargo
BuildRequires:  cargo-rpm-macros >= 24
BuildRequires:  openssl-devel
BuildRequires:  clang-devel

%description
API-compatible rewrite of https://github.com/scylladb/cpp-driver as a wrapper for Rust driver.

%package devel
Summary:        Development libraries for ${name}
Group:          Development/Tools
Requires:       %{name} = %{version}-%{release}
Requires:       rkgconfig

%description devel
Development libraries for %{name}

%prep
%autosetup
set -euo pipefail
%{__rm} -rf scylla-rust-wrapper/.cargo
%{__mkdir} -p scylla-rust-wrapper/.cargo
cat > scylla-rust-wrapper/.cargo/config << EOF
[build]
rustc = "%{__rustc}"
rustdoc = "%{__rustdoc}"

[profile.rpm]
inherits = "release"
opt-level = %{rustflags_opt_level}
codegen-units = %{rustflags_codegen_units}
debug = %{rustflags_debuginfo}
strip = "none"

[env]
CFLAGS = "%{build_cflags}"
CXXFLAGS = "%{build_cxxflags}"
LDFLAGS = "%{build_ldflags}"

[install]
root = "%{buildroot}%{_prefix}"

[term]
verbose = true
EOF

%build
(cd scylla-rust-wrapper && cargo build %{?_smp_mflags} --profile rpm)
sed -e "s#@prefix@#%{_prefix}#g" \
    -e "s#@exec_prefix@#%{_exec_prefix}#g" \
    -e "s#@libdir@#%{_libdir}#g" \
    -e "s#@includedir@#%{_includedir}#g" \
    -e "s#@version@#%{version}#g" \
    dist/common/pkgconfig/scylla_cpp_driver.pc.in > scylla_cpp_driver.pc
sed -e "s#@prefix@#%{_prefix}#g" \
    -e "s#@exec_prefix@#%{_exec_prefix}#g" \
    -e "s#@libdir@#%{_libdir}#g" \
    -e "s#@includedir@#%{_includedir}#g" \
    -e "s#@version@#%{version}#g" \
    dist/common/pkgconfig/scylla_cpp_driver_static.pc.in > scylla_cpp_driver_static.pc

%install
rm -rf %{buildroot}
install -Dpm0755 scylla-rust-wrapper/target/rpm/{*.so,*.a} -t %{buildroot}%{_libdir}
install -Dpm0644 *.pc -t %{buildroot}%{_libdir}/pkgconfig
install -Dpm0644 include/*.h -t %{buildroot}%{_includedir}/scylladb

%clean
rm -rf %{buildroot}

%post -p /sbin/ldconfig
%postun -p /sbin/ldconfig

%files
%defattr(-,root,root)
%doc README.md LICENSE
%{_libdir}/*.so

%files devel
%defattr(-,root,root)
%doc README.md LICENSE
%{_includedir}/scylladb/*.h
%{_libdir}/*.a
%{_libdir}/pkgconfig/*.pc

%changelog
* Thu Mar 28 2024 Takuya ASADA <syuu@scylladb.com>
- initial version of scylla-cpp-rust-driver.spec
