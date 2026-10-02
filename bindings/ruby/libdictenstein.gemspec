require_relative "lib/vinary_tree/libdictenstein/version"

Gem::Specification.new do |spec|
  spec.name = "libdictenstein"
  spec.version = VinaryTree::Libdictenstein::VERSION
  spec.authors = ["Dylon Edwards"]
  spec.email = ["dylon.devo@gmail.com"]
  spec.summary = "High-performance dictionaries and trie-maps for approximate string matching"
  spec.description = "High-performance dictionaries and trie-maps for approximate string matching."
  spec.homepage = "https://github.com/vinary-tree/libdictenstein"
  spec.license = "Apache-2.0"
  spec.required_ruby_version = ">= 3.3"
  spec.files = Dir["lib/**/*", "bin/*", "README.md", "LICENSE"]
  spec.bindir = "bin"
  spec.executables = ["libdictenstein-collection-profile"]
  spec.require_paths = ["lib"]
  spec.extra_rdoc_files = ["README.md"]
  spec.rdoc_options = ["--main", "README.md"]
  spec.metadata = {
    "source_code_uri" => spec.homepage,
    "documentation_uri" => "https://www.rubydoc.info/gems/libdictenstein/#{spec.version}",
    "rubygems_mfa_required" => "true"
  }
end
