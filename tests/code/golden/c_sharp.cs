using System;
using System.Collections.Generic;

namespace MyApp.Services;

public class UserService : BaseService, IDisposable
{
    private string _name;
    public UserService(string name) {}
    public void Dispose() {}
    public static string Format(int id) { return id.ToString(); }
}

public interface IRepository<T> : IEnumerable<T>
{
    T GetById(int id);
    void Save(T entity);
}

public enum Status
{
    Active,
    Inactive,
    Pending
}

public record Point(int X, int Y);
