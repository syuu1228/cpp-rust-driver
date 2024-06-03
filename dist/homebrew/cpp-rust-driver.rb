require "formula"

class CppRustDriver < Formula
  homepage "https://github.com/scylladb/cpp-rust-driver"
  head "https://github.com/syuu1228/cpp-rust-driver.git", branch: "homebrew_test"

  depends_on "openssl"
  depends_on "rust"

  def install
    chdir "scylla-rust-wrapper" do
      ENV["RUSTFLAGS"] = "-Clink-arg=-Wl,-install_name,#{prefix}/lib/libscylla-cpp-driver.2.dylib -Clink-arg=-Wl,-current_version,2.16.1 -Clink-arg=-Wl,-compatibility_version,2"
      system "cargo", "build", "--profile", "packaging", "--verbose"
      system "./versioning.sh", "--profile", "packaging"
      lib.install Dir["target/packaging/*.dylib","target/packaging/*.a"]
    end
    include.install "include/cassandra.h"
  end
end
