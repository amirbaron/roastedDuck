// Offline differential oracle; links only the frozen C++ reference, never roastedduck.
#include "duckdb/common/render_tree.hpp"
#include "duckdb/common/tree_renderer.hpp"
#include "duckdb/common/error_data.hpp"
#include "duckdb/common/enums/explain_format.hpp"
#include <iostream>
#include <sstream>
static std::string hex(const std::string &s) { const char *d="0123456789abcdef";std::string out;for(unsigned char c:s){out+=d[c>>4];out+=d[c&15];}return out; }
static std::string unhex(const std::string &s) { std::string out;for(size_t i=0;i<s.size();i+=2)out+=char(std::stoi(s.substr(i,2),nullptr,16));return out; }
int main() {
 using namespace duckdb;
 const ExplainFormat formats[]={ExplainFormat::TEXT,ExplainFormat::JSON,ExplainFormat::HTML,ExplainFormat::GRAPHVIZ,ExplainFormat::YAML,ExplainFormat::MERMAID};
 std::string line;
 while(std::getline(std::cin,line)) for(int shape=0;shape<2;shape++) for(int field=0;field<3;field++) for(int format=0;format<6;format++) {
  auto sample=unhex(line);
  RenderTree tree(shape?2:1,shape?2:1);InsertionOrderPreservingMap<string> params;
  params.insert(field==1?sample:"Key",field==2?sample:"Value");
  params.insert("kEY","duplicate"); // InsertionOrderPreservingMap is ASCII-CI and keeps the first value.
  params.insert("__estimated_cardinality__","1234");
  auto root=make_uniq<RenderTreeNode>(field==0?sample:"NODE",std::move(params));
  if(shape) {
   root->AddChildPosition(0,1);root->AddChildPosition(1,1);
   InsertionOrderPreservingMap<string> left,right;
   left.insert("__cardinality__","1000");left.insert("__timing__","0.12s");
   right.insert("__estimated_cardinality__","1");
   tree.SetNode(0,1,make_uniq<RenderTreeNode>("LEFT",std::move(left)));
   tree.SetNode(1,1,make_uniq<RenderTreeNode>("RIGHT",std::move(right)));
  }
  tree.SetNode(0,0,std::move(root));
  std::cout<<(shape?"tree:":"hex:")<<line<<'\t'<<field<<'\t'<<format<<'\t';
  try { std::ostringstream out;auto renderer=TreeRenderer::CreateRenderer(formats[format]);renderer->ToStream(tree,out);std::cout<<"ok\t"<<hex(out.str()); }
  catch(const std::exception &e){ErrorData error(e);std::cout<<"error\t"<<hex(error.RawMessage());}
  std::cout<<'\n';
 }
}
