#!/usr/bin/env ruby
# frozen_string_literal: true

require 'fileutils'
require 'json'
require 'optparse'
require 'open3'

options = { page: 'index', scope: 'hero' }
OptionParser.new do |parser|
  parser.banner = 'Usage: render.rb --liquid-root PATH --theme-root PATH --fixture PATH [--scope hero|template|page] [--output-dir PATH]'
  parser.on('--liquid-root PATH') { |value| options[:liquid_root] = value }
  parser.on('--theme-root PATH') { |value| options[:theme_root] = value }
  parser.on('--theme PATH') { |value| options[:theme_root] = value }
  parser.on('--fixture PATH') { |value| options[:fixture] = value }
  parser.on('--store PATH') { |value| options[:fixture] = value }
  parser.on('--page NAME') { |value| options[:page] = value }
  parser.on('--scope SCOPE') { |value| options[:scope] = value }
  parser.on('--only TYPE') { |value| options[:scope] = value }
  parser.on('--output-dir PATH') { |value| options[:output_dir] = value }
end.parse!
%i[liquid_root theme_root fixture].each { |key| abort "missing --#{key.to_s.tr('_', '-')}" unless options[key] }
$LOAD_PATH.unshift(File.join(File.expand_path(options[:liquid_root]), 'lib'))
require_relative 'renderer'

fixture = JSON.parse(File.read(options[:fixture]))
def verify_checkout(root, expected_sha, label)
  sha, status = Open3.capture2('git', '-C', root, 'rev-parse', 'HEAD')
  abort "#{label} SHA does not match fixture pin" unless status.success? && sha.strip == expected_sha
  tracked, status = Open3.capture2('git', '-C', root, 'status', '--porcelain', '--untracked-files=no')
  abort "#{label} checkout has tracked changes" unless status.success? && tracked.empty?
  sha.strip
end
verify_checkout(options[:theme_root], fixture.dig('theme', 'sha'), 'theme')
liquid_sha = verify_checkout(options[:liquid_root], '4e39ae4cc3da73921923c0669e0fc84a66b2f696', 'Ruby Liquid')
liquid_source = File.realpath(Liquid::Template.instance_method(:render).source_location.first)
abort 'Liquid loaded outside pinned checkout' unless liquid_source.start_with?("#{File.realpath(options[:liquid_root])}/lib/")
renderer = HorizonFixture::Renderer.new(theme_root: options[:theme_root], fixture: fixture)
html = renderer.render(page: options[:page], scope: options[:scope])
if options[:output_dir]
  FileUtils.mkdir_p(options[:output_dir])
  File.write(File.join(options[:output_dir], 'index.html'), html)
  File.write(File.join(options[:output_dir], 'styles.css'), renderer.stylesheets.values.join)
  manifest = renderer.manifest.merge('scope' => options[:scope], 'fixture_sha256' => Digest::SHA256.file(options[:fixture]).hexdigest, 'html_sha256' => Digest::SHA256.hexdigest(html), 'liquid_root' => File.realpath(options[:liquid_root]), 'liquid_sha' => liquid_sha, 'liquid_source' => liquid_source, 'ruby_version' => RUBY_VERSION)
  File.write(File.join(options[:output_dir], 'report.json'), JSON.pretty_generate(manifest) + "\n")
  puts JSON.generate(manifest)
else
  print html
end
