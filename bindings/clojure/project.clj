(defproject io.vinarytree/libdictenstein-clojure "4.0.0-rc.6"
  :description "High-performance dictionaries and trie-maps for approximate string matching."
  :url "https://github.com/vinary-tree/libdictenstein"
  :license {:name "Apache License 2.0"
            :url "https://www.apache.org/licenses/LICENSE-2.0.txt"}
  :scm {:name "git"
        :url "https://github.com/vinary-tree/libdictenstein"
        :connection "scm:git:https://github.com/vinary-tree/libdictenstein.git"
        :developerConnection "scm:git:ssh://git@github.com/vinary-tree/libdictenstein.git"}
  :pom-addition [:developers
                 [:developer
                  [:id "dylon"]
                  [:name "Dylon Edwards"]
                  [:email "dylon.devo@gmail.com"]]]
  :dependencies [[org.clojure/clojure "1.12.5" :scope "provided"]
                 [io.vinarytree/vinary-tree-interop "4.0.0-rc.6"]
                 [io.vinarytree/libdictenstein "4.0.0-rc.6"]]
  :profiles {:test {:dependencies [[org.clojure/data.json "2.5.2"]
                                   [io.vinarytree/liblevenshtein "4.0.0-rc.6"]]}}
  :source-paths ["src"]
  :test-paths ["test"]
  :jvm-opts ["--enable-native-access=ALL-UNNAMED"]
  :filespecs [{:type :path :path "../../LICENSE"}]
  :deploy-repositories
  [["clojars" {:url "https://repo.clojars.org"
                :username :env/clojars_username
                :password :env/clojars_password
                :sign-releases false}]])
